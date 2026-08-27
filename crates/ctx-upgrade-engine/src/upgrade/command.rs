//! Manual and automatic upgrade state machines.

#[cfg(unix)]
use std::env;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_platform::platform_security::{
    establish_private_data_root, verify_private_directory,
};

use super::download::DownloadedArtifact;
use super::install::managed_install_marker_for_current_exe;
use super::install::{
    absent_install_marker_error, apply_artifact, capture_install_snapshot,
    classify_repair_requirements, current_exe_is_unmanaged, current_install_path, pending_recovery,
    recover_interrupted_install, remove_terminal_recovery, ApplyResult, InstallRecovery,
    ManagedInstallMarker, PendingRecovery, TerminalRecovery,
};
#[cfg(unix)]
use super::install::{
    reexec_current_format_recovery, CurrentFormatRecoveryReexec, RECOVERY_REEXEC_ENV,
};
use super::metadata::{
    metadata_signature_url, metadata_url, parse_release_metadata, validate_artifact_url,
    verify_metadata_signature,
};
use super::state::{
    begin_automatic_attempt_locked, begin_manual_attempt_locked, begin_recovery_attempt_locked,
    reconcile_replacement_terminal_locked, try_acquire_automatic_upgrade,
    write_state_checked_locked, write_state_error_locked, write_state_phase_locked,
    AutomaticUpgradeLease, UpgradeAttempt, UpgradeLock,
};
use super::{
    automatic_upgrade_check_due, env_flag, platform_key, version_gt, AutomaticUpgradeObservation,
    AutomaticUpgradePolicyProvider, AutomaticUpgradePolicySnapshot, DaemonUpgradeLease,
    DaemonUpgradePort, SemanticAccelerator, SemanticLayoutPort, UpgradeEngine, UpgradeFailureKind,
    UpgradeObserver, UpgradePlan, UpgradePolicy, UpgradeTerminalStatus,
};
#[cfg(unix)]
use super::{is_valid_upgrade_attempt_id, ReleaseProcessPort};

mod daemon;
pub use daemon::PreparedAutomaticUpgrade;
use daemon::{finish_automatic_upgrade, prepare_automatic_upgrade};

const RELEASE_METADATA_MAX_BYTES: usize = 1024 * 1024;
const RELEASE_METADATA_SIGNATURE_MAX_BYTES: usize = 64 * 1024;
const RELEASE_ARTIFACT_MAX_BYTES: usize = 128 * 1024 * 1024;
const RELEASE_ONNXRUNTIME_ARTIFACT_MAX_BYTES: usize = 1024 * 1024 * 1024;
const SEMANTIC_MODEL_ARCHIVE_MAX_BYTES: u64 = 768 * 1024 * 1024;
const SEMANTIC_CPU_ARCHIVE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SEMANTIC_ACCELERATOR_ARCHIVE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const RELEASE_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const CURRENT_FORMAT_ROLLBACK_DETAIL: &str =
    "schema-2 interrupted ctx installation was rolled back to its identity-validated current-format executable; recovery must fix forward";

#[cfg(unix)]
fn continue_current_format_recovery_reexec<L: DaemonUpgradeLease>(
    process: &dyn ReleaseProcessPort,
    handoff: L,
    recovery: CurrentFormatRecoveryReexec,
) -> Result<()> {
    handoff
        .release_for_current_format_reexec()
        .context("preserve daemon restart intent for current-format recovery re-exec")?;
    reexec_current_format_recovery(process, recovery)
}

#[derive(Debug, Clone)]
pub struct UpgradeOutcome {
    command: &'static str,
    status: &'static str,
    message: String,
    plan: Option<UpgradePlan>,
    applied: bool,
    dry_run: bool,
    warnings: Vec<String>,
    attempt_id: Option<String>,
}

impl UpgradeOutcome {
    pub fn command(&self) -> &'static str {
        self.command
    }

    pub fn status(&self) -> &'static str {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn plan(&self) -> Option<&UpgradePlan> {
        self.plan.as_ref()
    }

    pub fn applied(&self) -> bool {
        self.applied
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn attempt_id(&self) -> Option<&str> {
        self.attempt_id.as_deref()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        command: &'static str,
        status: &'static str,
        message: &str,
        applied: bool,
    ) -> Self {
        Self {
            command,
            status,
            message: message.to_owned(),
            plan: None,
            applied,
            dry_run: false,
            warnings: Vec::new(),
            attempt_id: None,
        }
    }
}

impl<D: DaemonUpgradePort + ?Sized> UpgradeEngine<'_, D> {
    pub fn prepare_data_root(&self, data_root: &Path) -> Result<()> {
        prepare_upgrade_data_root(data_root)
    }

    pub fn check(
        &self,
        data_root: &Path,
        policy: UpgradePolicy<'_>,
        channel_override: Option<&str>,
    ) -> Result<UpgradeOutcome> {
        check_upgrade(self, data_root, policy, channel_override, "upgrade_check")
    }

    pub fn apply(
        &self,
        data_root: &Path,
        policy: UpgradePolicy<'_>,
        channel_override: Option<&str>,
        dry_run: bool,
    ) -> Result<UpgradeOutcome> {
        apply_upgrade(self, data_root, policy, channel_override, dry_run)
    }

    #[cfg(windows)]
    pub fn run_replacement_helper(
        &self,
        install_path: &Path,
        attempt_id: &str,
        parent_pid: u32,
    ) -> Result<()> {
        match super::install::run_replacement_helper(
            self.semantic_layout,
            self.daemon,
            install_path,
            attempt_id,
            parent_pid,
        )? {
            super::install::HelperOutcome::Applied { .. } => Ok(()),
            super::install::HelperOutcome::Failed { error } => Err(anyhow!(error)),
        }
    }

    pub fn prepare_automatic<P, O>(
        &self,
        policy_provider: &P,
        observer: &O,
        data_root: &Path,
        startup_policy: &P::Snapshot,
    ) -> Result<Option<PreparedAutomaticUpgrade>>
    where
        P: AutomaticUpgradePolicyProvider,
        O: UpgradeObserver<P::Snapshot>,
    {
        prepare_automatic_upgrade(self, policy_provider, observer, data_root, startup_policy)
    }

    pub fn finish_automatic<P, O>(
        &self,
        policy_provider: &P,
        observer: &O,
        prepared: PreparedAutomaticUpgrade,
        handoff: Option<D::Lease>,
    ) -> Result<()>
    where
        P: AutomaticUpgradePolicyProvider,
        O: UpgradeObserver<P::Snapshot>,
    {
        finish_automatic_upgrade(self, policy_provider, observer, prepared, handoff)
    }

    /// Run one complete automatic attempt from a detached command worker.
    /// The persistent daemon enters through the same installation scheduler.
    pub fn run_automatic<P, O>(
        &self,
        policy_provider: &P,
        observer: &O,
        data_root: &Path,
        startup_policy: &P::Snapshot,
    ) -> Result<()>
    where
        P: AutomaticUpgradePolicyProvider,
        O: UpgradeObserver<P::Snapshot>,
    {
        let Some(prepared) =
            self.prepare_automatic(policy_provider, observer, data_root, startup_policy)?
        else {
            return Ok(());
        };
        let attempt_id = prepared
            .attempt_id()
            .ok_or_else(|| anyhow!("automatic upgrade prepared without an attempt identity"))?
            .to_owned();
        let owner_root = prepared.data_root().to_path_buf();
        let handoff = match self.daemon.begin(&owner_root, &attempt_id) {
            Ok(handoff) => handoff,
            Err(error) => {
                let abort_error = prepared.abort(&error).err();
                return Err(match abort_error {
                    Some(abort_error) => error.context(format!(
                        "also failed to terminalize automatic upgrade state: {abort_error:#}"
                    )),
                    None => error,
                });
            }
        };
        self.finish_automatic(policy_provider, observer, prepared, Some(handoff))
    }
}

fn upgrade_failure_kind(error: &anyhow::Error) -> UpgradeFailureKind {
    let text = format!("{error:#}").to_ascii_lowercase();
    if text.contains("upgrade lock") {
        UpgradeFailureKind::LockFailed
    } else if text.contains("not installed by the hosted installer")
        || text.contains("install marker")
        || text.contains("unmanaged")
    {
        UpgradeFailureKind::UnmanagedInstall
    } else if text.contains("metadata") && text.contains("download") {
        UpgradeFailureKind::MetadataFetch
    } else if text.contains("signature") {
        UpgradeFailureKind::SignatureVerify
    } else if text.contains("metadata") {
        UpgradeFailureKind::MetadataInvalid
    } else if text.contains("checksum") || text.contains("sha") {
        UpgradeFailureKind::ArtifactVerify
    } else if text.contains("download") {
        UpgradeFailureKind::ArtifactDownload
    } else if text.contains("does not allow") {
        UpgradeFailureKind::PolicyDisallowed
    } else {
        UpgradeFailureKind::ApplyFailed
    }
}

fn prepare_upgrade_data_root(data_root: &Path) -> Result<()> {
    establish_private_data_root(data_root).with_context(|| {
        format!(
            "establish private upgrade data root {}",
            data_root.display()
        )
    })?;
    verify_private_directory(data_root)
        .with_context(|| format!("verify private upgrade data root {}", data_root.display()))
}

fn semantic_accelerator(
    semantic_layout: &dyn SemanticLayoutPort,
    platform: &str,
) -> Result<Option<SemanticAccelerator>> {
    let accelerator = semantic_layout.native_accelerator();
    if !matches!(
        (platform, accelerator),
        ("macos-arm64", Some(SemanticAccelerator::CoreMl))
            | ("windows-x64", Some(SemanticAccelerator::WindowsMl))
            | ("linux-x64", Some(SemanticAccelerator::OrtCuda))
            | (_, None)
    ) {
        return Err(anyhow!(
            "detected Semantic accelerator is incompatible with {platform}"
        ));
    }
    Ok(accelerator)
}

fn semantic_archive_download_limit(asset: &super::metadata::SemanticAssetMetadata) -> Result<u64> {
    match asset.role.as_str() {
        "model" => Ok(SEMANTIC_MODEL_ARCHIVE_MAX_BYTES),
        "cpu-runtime" => Ok(SEMANTIC_CPU_ARCHIVE_MAX_BYTES),
        "accelerator" => Ok(SEMANTIC_ACCELERATOR_ARCHIVE_MAX_BYTES),
        role => Err(anyhow!(
            "signed Semantic provisioning contains unsupported role {role}"
        )),
    }
}

fn check_upgrade<D: DaemonUpgradePort + ?Sized>(
    engine: &UpgradeEngine<'_, D>,
    data_root: &Path,
    policy: UpgradePolicy<'_>,
    channel_override: Option<&str>,
    command: &'static str,
) -> Result<UpgradeOutcome> {
    if let Some(recovery) = pending_recovery(data_root, engine.semantic_layout)? {
        if let Some(terminal) = recovery.terminal.as_ref() {
            let lock = UpgradeLock::acquire_terminal_recovery(&recovery, engine.semantic_layout)?;
            let (applied, detail) = match terminal {
                TerminalRecovery::Applied { warning } => (true, warning.as_deref()),
                TerminalRecovery::Failed { error } => (false, Some(error.as_str())),
            };
            reconcile_replacement_terminal_locked(
                &lock,
                &recovery.attempt_id,
                applied,
                detail,
                policy.interval,
            )?;
            remove_terminal_recovery(&recovery, lock.installation(), engine.semantic_layout)?;
        } else {
            #[cfg(windows)]
            return Err(anyhow!(
                "interrupted Windows installation requires `ctx upgrade` so daemon handoff and replacement recovery remain coordinated"
            ));
            #[cfg(not(windows))]
            {
                let recovery_lock =
                    UpgradeLock::acquire_recovery(&recovery, engine.semantic_layout)?;
                begin_recovery_attempt_locked(
                    &recovery_lock,
                    &recovery.attempt_id,
                    "manual_recovery",
                )?;
                let handoff = engine
                    .daemon
                    .begin(&recovery.data_root, &recovery.attempt_id)?;
                match recover_interrupted_install(
                    engine.process,
                    &recovery,
                    recovery_lock.installation(),
                    engine.semantic_layout,
                )? {
                    InstallRecovery::None => {
                        return Err(anyhow!(
                            "interrupted ctx installation recovery disappeared while owned"
                        ));
                    }
                    InstallRecovery::Recovered { committed } => {
                        reconcile_replacement_terminal_locked(
                            &recovery_lock,
                            &recovery.attempt_id,
                            committed,
                            (!committed).then_some(CURRENT_FORMAT_ROLLBACK_DETAIL),
                            policy.interval,
                        )?;
                        drop(recovery_lock);
                        handoff.resume_with(&current_install_path()?)?;
                    }
                    #[cfg(unix)]
                    InstallRecovery::ReexecCurrentFormat(reexec) => {
                        reconcile_replacement_terminal_locked(
                            &recovery_lock,
                            &recovery.attempt_id,
                            false,
                            Some(CURRENT_FORMAT_ROLLBACK_DETAIL),
                            policy.interval,
                        )?;
                        drop(recovery_lock);
                        continue_current_format_recovery_reexec(engine.process, handoff, reexec)?;
                        unreachable!("successful recovery re-exec does not return");
                    }
                }
            }
        }
    }
    // Unmanaged installations have no installation lock or scheduler state
    // beside the executable: the check is lock-free and stateless so
    // read-only package-manager directories keep working.
    if current_exe_is_unmanaged() {
        let plan = build_upgrade_plan(engine, policy, channel_override, false)?;
        return Ok(check_outcome(command, plan, None));
    }
    let lock = UpgradeLock::acquire(data_root)?;
    let attempt = begin_manual_attempt_locked(data_root, &lock, command)?;
    let plan = match build_upgrade_plan(engine, policy, channel_override, false) {
        Ok(plan) => plan,
        Err(error) => {
            let _ = write_state_error_locked(
                data_root,
                &lock,
                &attempt,
                "failed",
                &format!("{error:#}"),
            );
            return Err(error);
        }
    };
    let status = if plan.update_available {
        "available"
    } else {
        "up_to_date"
    };
    write_state_checked_locked(data_root, &lock, &attempt, &plan, status, policy.interval)?;
    Ok(check_outcome(command, plan, Some(attempt.id().to_owned())))
}

fn check_outcome(
    command: &'static str,
    plan: UpgradePlan,
    attempt_id: Option<String>,
) -> UpgradeOutcome {
    let status = if plan.update_available {
        "available"
    } else {
        "up_to_date"
    };
    let message = if plan.update_available {
        format!(
            "ctx {} is available (current {}, channel {}).",
            plan.latest_version, plan.current_version, plan.channel
        )
    } else {
        format!("ctx {} is up to date.", plan.current_version)
    };
    let warnings = plan.warnings.clone();
    UpgradeOutcome {
        command,
        status,
        message,
        plan: Some(plan),
        applied: false,
        dry_run: false,
        warnings,
        attempt_id,
    }
}

fn apply_upgrade<D: DaemonUpgradePort + ?Sized>(
    engine: &UpgradeEngine<'_, D>,
    data_root: &Path,
    policy: UpgradePolicy<'_>,
    channel_override: Option<&str>,
    dry_run: bool,
) -> Result<UpgradeOutcome> {
    if let Some(recovery) = pending_recovery(data_root, engine.semantic_layout)? {
        if let Some(terminal) = recovery.terminal.as_ref() {
            let lock = UpgradeLock::acquire_terminal_recovery(&recovery, engine.semantic_layout)?;
            let (applied, detail) = match terminal {
                TerminalRecovery::Applied { warning } => (true, warning.as_deref()),
                TerminalRecovery::Failed { error } => (false, Some(error.as_str())),
            };
            reconcile_replacement_terminal_locked(
                &lock,
                &recovery.attempt_id,
                applied,
                detail,
                policy.interval,
            )?;
            remove_terminal_recovery(&recovery, lock.installation(), engine.semantic_layout)?;
            drop(lock);
        } else {
            let recovery_attempt_id = recovery.attempt_id.clone();
            let origin_root = recovery.data_root.clone();
            let recovery_lock = UpgradeLock::acquire_recovery(&recovery, engine.semantic_layout)?;
            begin_recovery_attempt_locked(&recovery_lock, &recovery_attempt_id, "manual_recovery")?;
            let daemon_handoff = engine.daemon.begin(&origin_root, &recovery_attempt_id)?;
            match recover_interrupted_install(
                engine.process,
                &recovery,
                recovery_lock.installation(),
                engine.semantic_layout,
            )? {
                InstallRecovery::None => {
                    return Err(anyhow!(
                        "interrupted ctx installation recovery disappeared while owned"
                    ));
                }
                InstallRecovery::Recovered { committed } => {
                    reconcile_replacement_terminal_locked(
                        &recovery_lock,
                        &recovery_attempt_id,
                        committed,
                        (!committed).then_some(CURRENT_FORMAT_ROLLBACK_DETAIL),
                        policy.interval,
                    )?;
                    drop(recovery_lock);
                    if let Err(error) = daemon_handoff.resume_with(&current_install_path()?) {
                        if !committed {
                            return Err(error);
                        }
                        let warning = format!(
                        "ctx upgrade was already committed, but daemon restart remains pending: {error:#}"
                    );
                        return Ok(UpgradeOutcome {
                        command: "upgrade",
                        status: "applied",
                        message:
                            "recovered a committed ctx installation; daemon restart remains pending"
                                .to_owned(),
                        plan: None,
                        applied: true,
                        dry_run: false,
                        warnings: vec![warning],
                        attempt_id: Some(recovery_attempt_id),
                    });
                    }
                    if crate::upgrade::test_harness_enabled()
                        && env_flag("CTX_UPGRADE_STOP_AFTER_RECOVERY_FOR_TESTS")
                    {
                        return Err(anyhow!("stopped after interrupted install recovery"));
                    }
                }
                #[cfg(windows)]
                InstallRecovery::Scheduled {
                    attempt_id,
                    helper_pid,
                } => {
                    daemon_handoff.transfer_to_replacement_helper(helper_pid)?;
                    drop(recovery_lock);
                    return Ok(UpgradeOutcome {
                    command: "upgrade",
                    status: "scheduled",
                    message: format!(
                        "rescheduled interrupted ctx replacement attempt {attempt_id}; it will finish after this process exits"
                    ),
                    plan: None,
                    applied: false,
                    dry_run: false,
                    warnings: Vec::new(),
                    attempt_id: Some(attempt_id),
                });
                }
                #[cfg(unix)]
                InstallRecovery::ReexecCurrentFormat(reexec) => {
                    reconcile_replacement_terminal_locked(
                        &recovery_lock,
                        &recovery_attempt_id,
                        false,
                        Some(CURRENT_FORMAT_ROLLBACK_DETAIL),
                        policy.interval,
                    )?;
                    drop(recovery_lock);
                    continue_current_format_recovery_reexec(
                        engine.process,
                        daemon_handoff,
                        reexec,
                    )?;
                    unreachable!("successful recovery re-exec does not return");
                }
            }
        }
    }
    #[cfg(unix)]
    if env::var(RECOVERY_REEXEC_ENV)
        .ok()
        .is_some_and(|attempt_id| is_valid_upgrade_attempt_id(&attempt_id))
    {
        env::remove_var(RECOVERY_REEXEC_ENV);
    }
    // Unmanaged installations cannot self-upgrade. Fail with the conversion
    // guidance before acquiring any installation-scoped state so read-only
    // package-manager directories report the same actionable error.
    if current_exe_is_unmanaged() {
        return Err(absent_install_marker_error());
    }
    let upgrade_lock = UpgradeLock::acquire(data_root)?;
    let attempt = begin_manual_attempt_locked(data_root, &upgrade_lock, "manual_apply")?;
    let result = (|| -> Result<UpgradeOutcome> {
        let plan = build_upgrade_plan(engine, policy, channel_override, true)?;
        let repairs = classify_repair_requirements(
            engine.semantic_layout,
            &plan,
            data_root,
            policy.semantic_enabled,
        )?;
        if !plan.update_available && !repairs.any() {
            write_state_checked_locked(
                data_root,
                &upgrade_lock,
                &attempt,
                &plan,
                "up_to_date",
                policy.interval,
            )?;
            let warnings = plan.warnings.clone();
            return Ok(UpgradeOutcome {
                command: "upgrade",
                status: "up_to_date",
                message: format!("ctx {} is already installed.", plan.current_version),
                plan: Some(plan),
                applied: false,
                dry_run,
                warnings,
                attempt_id: Some(attempt.id().to_owned()),
            });
        }
        if plan.update_available && !plan.metadata.self_upgrade_allowed {
            return Err(anyhow!(
                "release {} does not allow self-upgrade",
                plan.latest_version
            ));
        }
        if plan.update_available
            && plan.semantic_provisioning.is_none()
            && plan.metadata.onnxruntime.is_none()
        {
            return Err(anyhow!(
                "release {} is newer than this ctx build but has no complete ONNX Runtime sidecar metadata; refusing a downgrade-compatible legacy update",
                plan.latest_version
            ));
        }
        if dry_run {
            write_state_checked_locked(
                data_root,
                &upgrade_lock,
                &attempt,
                &plan,
                "dry_run",
                policy.interval,
            )?;
            let warnings = plan.warnings.clone();
            return Ok(UpgradeOutcome {
                command: "upgrade",
                status: "dry_run",
                message: if plan.update_available {
                    format!(
                        "ctx {} would upgrade to {}.",
                        plan.current_version, plan.latest_version
                    )
                } else if repairs.legacy_runtime {
                    format!(
                        "ctx {} would repair its signed legacy ONNX Runtime installation.",
                        plan.current_version
                    )
                } else {
                    format!(
                        "ctx {} would provision signed Semantic model and runtime assets.",
                        plan.current_version
                    )
                },
                plan: Some(plan),
                applied: false,
                dry_run: true,
                warnings,
                attempt_id: Some(attempt.id().to_owned()),
            });
        }
        let mut artifact = if plan.update_available {
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
        let mut runtime_artifact = if (plan.update_available || repairs.legacy_runtime)
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
                (None, None) => None,
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
        write_state_phase_locked(&upgrade_lock, &attempt, "quiescing")?;
        let daemon_handoff = engine.daemon.begin(data_root, attempt.id())?;
        let daemon_restart = daemon_handoff.replacement_restart();
        let mut before_publish = || Ok(());
        let apply_result = match apply_artifact(
            engine.process,
            engine.semantic_layout,
            upgrade_lock.installation(),
            &plan,
            artifact.as_mut(),
            runtime_artifact.as_mut(),
            &mut semantic_artifacts,
            data_root,
            attempt.id(),
            daemon_restart.map(|restart| (restart.trigger, restart.loop_interval_seconds)),
            &mut before_publish,
        ) {
            Ok(result) => result,
            Err(error) => {
                let restart = daemon_handoff.resume_with(&plan.install_path);
                return match restart {
                    Ok(()) => Err(error),
                    Err(restart_error) => Err(error.context(format!(
                        "also failed to resume daemon lifecycle after upgrade failure: {restart_error:#}"
                    ))),
                };
            }
        };
        let mut warnings = plan.warnings.clone();
        if let ApplyResult::Scheduled { helper_pid } = apply_result {
            if let Err(error) = daemon_handoff.transfer_to_replacement_helper(helper_pid) {
                warnings.push(format!(
                    "replacement helper is ready, but daemon handoff bookkeeping remains pending: {error:#}"
                ));
            }
            record_post_apply_state(
                data_root,
                &upgrade_lock,
                &attempt,
                &plan,
                "scheduled",
                policy.interval,
                &mut warnings,
            );
            let message = if plan.update_available {
                format!(
                    "scheduled ctx {} -> {} at {}; replacement will finish after this process exits",
                    plan.current_version,
                    plan.latest_version,
                    plan.install_path.display()
                )
            } else if repairs.legacy_runtime {
                "scheduled signed legacy ONNX Runtime repair; replacement will finish after this process exits"
                    .to_owned()
            } else {
                "scheduled signed Semantic asset repair; replacement will finish after this process exits"
                    .to_owned()
            };
            return Ok(UpgradeOutcome {
                command: "upgrade",
                status: "scheduled",
                message,
                plan: Some(plan),
                applied: false,
                dry_run: false,
                warnings,
                attempt_id: Some(attempt.id().to_owned()),
            });
        }
        if let Some(warning) = apply_result.cleanup_warning() {
            warnings.push(warning.to_owned());
        }
        record_post_apply_state(
            data_root,
            &upgrade_lock,
            &attempt,
            &plan,
            "applied",
            policy.interval,
            &mut warnings,
        );
        // Filesystem publication is the commit point.  A daemon restart is a
        // follow-up operation: report it for retry, but never turn a committed
        // upgrade into scheduler failure/backoff.
        if let Err(error) = daemon_handoff.resume_with(&plan.install_path) {
            warnings.push(format!(
                "ctx upgrade applied, but daemon restart is pending: {error:#}"
            ));
        }
        let message = if plan.update_available {
            format!(
                "upgraded ctx {} -> {} at {}",
                plan.current_version,
                plan.latest_version,
                plan.install_path.display()
            )
        } else if repairs.legacy_runtime {
            format!(
                "repaired signed legacy ONNX Runtime installation for ctx {}",
                plan.current_version
            )
        } else {
            format!(
                "provisioned signed Semantic model and runtime assets for ctx {}",
                plan.current_version
            )
        };
        Ok(UpgradeOutcome {
            command: "upgrade",
            status: "applied",
            message,
            plan: Some(plan),
            applied: true,
            dry_run: false,
            warnings,
            attempt_id: Some(attempt.id().to_owned()),
        })
    })();
    if let Err(error) = &result {
        let _ = write_state_error_locked(
            data_root,
            &upgrade_lock,
            &attempt,
            "failed",
            &format!("{error:#}"),
        );
    }
    result
}

fn record_post_apply_state(
    data_root: &Path,
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
    plan: &UpgradePlan,
    status: &str,
    interval: std::time::Duration,
    warnings: &mut Vec<String>,
) {
    if let Err(error) = write_state_checked_locked(data_root, lock, attempt, plan, status, interval)
    {
        warnings.push(format!(
            "upgrade {status}, but local upgrade state could not be written: {error:#}"
        ));
    }
}

fn build_upgrade_plan<D: DaemonUpgradePort + ?Sized>(
    engine: &UpgradeEngine<'_, D>,
    policy: UpgradePolicy<'_>,
    channel_override: Option<&str>,
    require_managed: bool,
) -> Result<UpgradePlan> {
    let fallback_current_version = engine.identity.version().to_owned();
    let platform = platform_key()?.to_owned();
    let channel = channel_override
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(policy.channel)
        .to_owned();
    let mut warnings = Vec::new();
    let snapshot = capture_install_snapshot(
        require_managed,
        &platform,
        &channel,
        &fallback_current_version,
        &mut warnings,
    )?;
    if snapshot.marker.staging_dogfood {
        return Err(anyhow!(
            "this staging dogfood ctx installation is isolated from release upgrades"
        ));
    }
    let current_version = snapshot.marker.version.clone();
    let managed = warnings.is_empty();
    let metadata_url = metadata_url(&channel);
    let signature_url = metadata_signature_url(&metadata_url);
    let metadata_bytes = engine
        .transport
        .get_bytes_limited(&metadata_url, RELEASE_METADATA_MAX_BYTES)
        .with_context(|| format!("download release metadata {metadata_url}"))?;
    let signature_bytes = engine
        .transport
        .get_bytes_limited(&signature_url, RELEASE_METADATA_SIGNATURE_MAX_BYTES)
        .with_context(|| format!("download release metadata signature {signature_url}"))?;
    verify_metadata_signature(&metadata_bytes, &signature_bytes)?;
    let metadata = parse_release_metadata(
        &metadata_bytes,
        &platform,
        &channel,
        policy.semantic_enabled,
        engine.semantic_layout,
    )?;
    let artifact_url = format!(
        "{}/{}",
        metadata.base_url.trim_end_matches('/'),
        metadata.artifact
    );
    validate_artifact_url(&metadata.base_url, &metadata.artifact)?;
    if let Some(runtime) = &metadata.onnxruntime {
        validate_artifact_url(&metadata.base_url, &runtime.artifact)?;
    }
    let accelerator = if metadata.semantic.is_some() {
        semantic_accelerator(engine.semantic_layout, &platform)?
    } else {
        None
    };
    let semantic_provisioning = metadata
        .semantic
        .as_ref()
        .map(|semantic| semantic.select(&platform, accelerator))
        .transpose()?;
    if let Some(provisioning) = &semantic_provisioning {
        for asset in &provisioning.assets {
            validate_artifact_url(&metadata.base_url, &asset.metadata.artifact)?;
        }
    }
    let update_available = version_gt(&metadata.version, &current_version);
    Ok(UpgradePlan {
        current_version,
        latest_version: metadata.version.clone(),
        channel,
        platform,
        metadata_url,
        artifact_url,
        artifact_sha256: metadata.sha256.clone(),
        install_path: snapshot.marker.install_path.clone(),
        install_fingerprint: snapshot.fingerprint,
        update_available,
        managed,
        warnings,
        metadata,
        semantic_provisioning,
    })
}
