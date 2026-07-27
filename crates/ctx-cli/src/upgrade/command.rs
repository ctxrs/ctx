use std::{
    env,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use ctx_history_core::platform_security::{create_private_directory_all, verify_private_directory};
use serde_json::{json, Value};

use crate::{
    analytics::{
        self, count_bucket, OperationCompletedV1, Outcome, PublicEventV1, UpgradeChannel,
        UpgradeFailureKind, UpgradeMode, UpgradeOperation, UpgradeStatus, UpgradeTelemetry,
    },
    config::AppConfig,
    net,
    semantic::{semantic_native_accelerator_target, SemanticNativeAcceleratorTarget},
};

use super::download::DownloadedArtifact;
use super::install::{
    apply_artifact, capture_install_snapshot, current_install_path, pending_recovery,
    recover_interrupted_install, reexec_recovered_executable, remove_terminal_recovery,
    semantic_install_required, ApplyResult, InstallRecovery, PendingRecovery, TerminalRecovery,
    RECOVERY_REEXEC_ENV,
};
use super::metadata::{
    metadata_signature_url, metadata_url, parse_release_metadata, validate_artifact_url,
    verify_metadata_signature, SemanticAccelerator,
};
use super::path::path_diagnostics;
use super::state::{
    begin_manual_attempt_locked, begin_recovery_attempt_locked, claim_daemon_auto_upgrade,
    reconcile_replacement_terminal_locked, set_auto_mode, write_state_checked_locked,
    write_state_error_locked, write_state_phase_locked, AutoUpgradeClaim, UpgradeAttempt,
    UpgradeLock,
};
use super::{env_flag, is_valid_upgrade_attempt_id, platform_key, version_gt, UpgradePlan};

mod daemon;
pub(crate) use daemon::{
    finish_daemon_auto_upgrade, prepare_daemon_auto_upgrade, PreparedDaemonUpgrade,
};
mod status;
use status::render_status;

const RELEASE_METADATA_MAX_BYTES: usize = 1024 * 1024;
const RELEASE_METADATA_SIGNATURE_MAX_BYTES: usize = 64 * 1024;
const RELEASE_ARTIFACT_MAX_BYTES: usize = 128 * 1024 * 1024;
const RELEASE_ONNXRUNTIME_ARTIFACT_MAX_BYTES: usize = 1024 * 1024 * 1024;
const SEMANTIC_MODEL_ARCHIVE_MAX_BYTES: u64 = 768 * 1024 * 1024;
const SEMANTIC_CPU_ARCHIVE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const SEMANTIC_ACCELERATOR_ARCHIVE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const RELEASE_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Debug, Args)]
pub struct UpgradeArgs {
    #[command(subcommand)]
    pub command: Option<UpgradeCommand>,
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long, hide = true)]
    pub replacement_helper: bool,
    #[arg(long, hide = true)]
    pub install_path: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub attempt_id: Option<String>,
    #[arg(long, hide = true)]
    pub parent_pid: Option<u32>,
}

#[derive(Debug, Subcommand)]
pub enum UpgradeCommand {
    #[command(about = "Check whether a newer ctx release is available")]
    Check(UpgradeCheckArgs),
    #[command(about = "Show local upgrade state")]
    Status(UpgradeStatusArgs),
    #[command(about = "Enable daemon-owned automatic upgrades")]
    Enable,
    #[command(about = "Disable daemon-owned automatic upgrades")]
    Disable,
}

#[derive(Debug, Args)]
pub struct UpgradeCheckArgs {
    #[arg(long)]
    pub channel: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UpgradeStatusArgs {
    #[arg(long)]
    pub json: bool,
}

impl UpgradeArgs {
    pub fn json_output(&self) -> bool {
        self.json
            || matches!(
                &self.command,
                Some(UpgradeCommand::Check(args)) if args.json
            )
            || matches!(
                &self.command,
                Some(UpgradeCommand::Status(args)) if args.json
            )
            || self.replacement_helper
    }

    pub fn operation(&self) -> &'static str {
        match &self.command {
            Some(UpgradeCommand::Check(_)) => "check",
            Some(UpgradeCommand::Status(_)) => "status",
            Some(UpgradeCommand::Enable) => "enable",
            Some(UpgradeCommand::Disable) => "disable",
            None => "apply",
        }
    }
}

#[derive(Debug, Clone)]
struct UpgradeOutcome {
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
    fn json(&self) -> Value {
        let plan = self.plan.as_ref();
        json!({
            "schema_version": 1,
            "command": self.command,
            "ok": true,
            "status": self.status,
            "message": self.message,
            "current_version": plan.map(|plan| if self.applied {
                plan.latest_version.as_str()
            } else {
                plan.current_version.as_str()
            }),
            "latest_version": plan.map(|plan| plan.latest_version.as_str()),
            "update_available": plan
                .map(|plan| !self.applied && plan.update_available)
                .unwrap_or(false),
            "update_was_available": plan.map(|plan| plan.update_available).unwrap_or(false),
            "channel": plan.map(|plan| plan.channel.as_str()),
            "platform": plan.map(|plan| plan.platform.as_str()),
            "metadata_url": plan.map(|plan| plan.metadata_url.as_str()),
            "artifact_url": plan.map(|plan| plan.artifact_url.as_str()),
            "install_path": plan.map(|plan| plan.install_path.display().to_string()),
            "managed": plan.map(|plan| plan.managed).unwrap_or(false),
            "path": plan.map(|plan| plan.path.json()),
            "applied": self.applied,
            "dry_run": self.dry_run,
            "warnings": self.warnings,
            "upgrade_attempt_id": self.attempt_id,
        })
    }
}

pub fn run(
    args: UpgradeArgs,
    data_root: PathBuf,
    config: AppConfig,
    telemetry: &mut UpgradeTelemetry,
) -> Result<()> {
    #[cfg(windows)]
    if args.replacement_helper {
        let install_path = args
            .install_path
            .as_deref()
            .ok_or_else(|| anyhow!("replacement helper missing --install-path"))?;
        let attempt_id = args
            .attempt_id
            .as_deref()
            .ok_or_else(|| anyhow!("replacement helper missing --attempt-id"))?;
        telemetry.suppress_event = true;
        let outcome = super::install::run_replacement_helper(
            install_path,
            attempt_id,
            args.parent_pid.unwrap_or(0),
        )?;
        match outcome {
            super::install::HelperOutcome::Applied { .. } => return Ok(()),
            super::install::HelperOutcome::Failed { error, .. } => return Err(anyhow!(error)),
        }
    }
    if let Err(error) = prepare_upgrade_data_root(&data_root) {
        // Analytics identity creation writes beneath the data root and would
        // otherwise repair an insecure pre-existing root after any upgrade
        // operation, including the read-only status command, rejected it.
        telemetry.suppress_event = true;
        return Err(error);
    }
    let result = (|| -> Result<()> {
        match &args.command {
            Some(UpgradeCommand::Check(check)) => {
                let channel = check.channel.as_deref().or(args.channel.as_deref());
                let outcome = check_upgrade(&data_root, &config, channel, "upgrade_check")?;
                insert_upgrade_outcome_analytics(telemetry, &outcome);
                render_outcome(&outcome, check.json || args.json)
            }
            Some(UpgradeCommand::Status(status)) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::StatusChecked);
                render_status(&data_root, &config, status.json || args.json)
            }
            Some(UpgradeCommand::Enable) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::AutoEnabled);
                set_auto_mode(&data_root, "apply")
            }
            Some(UpgradeCommand::Disable) => {
                insert_upgrade_simple_analytics(telemetry, UpgradeStatus::AutoDisabled);
                set_auto_mode(&data_root, "off")
            }
            None => {
                let outcome =
                    apply_upgrade(&data_root, &config, args.channel.as_deref(), args.dry_run)?;
                insert_upgrade_outcome_analytics(telemetry, &outcome);
                render_outcome(&outcome, args.json)
            }
        }
    })();
    if let Err(error) = &result {
        insert_upgrade_error_analytics(telemetry, error);
    }
    result
}

fn insert_upgrade_outcome_analytics(telemetry: &mut UpgradeTelemetry, outcome: &UpgradeOutcome) {
    telemetry.status = Some(UpgradeStatus::from_safe_summary(outcome.status));
    telemetry.applied = Some(outcome.applied);
    telemetry.scheduled = Some(outcome.status == "scheduled");
    telemetry.update_available = Some(false);
    telemetry.update_was_available = Some(false);
    telemetry.upgrade_attempt_id = outcome.attempt_id.clone();
    telemetry.managed_install = Some(false);
    telemetry.self_upgrade_allowed = Some(false);
    telemetry.auto_upgrade_allowed = Some(false);
    telemetry.warning_count = Some(count_bucket(outcome.warnings.len() as u64));
    if let Some(plan) = &outcome.plan {
        telemetry.channel = Some(UpgradeChannel::from_config(&plan.channel));
        // The plan retains pre-apply availability for update_was_available analytics.
        // A completed apply is no longer pending an update.
        telemetry.update_available = Some(if outcome.applied {
            false
        } else {
            plan.update_available
        });
        telemetry.update_was_available = Some(plan.update_available);
        telemetry.managed_install = Some(plan.managed);
        telemetry.self_upgrade_allowed = Some(plan.metadata.self_upgrade_allowed);
        telemetry.auto_upgrade_allowed = Some(plan.metadata.auto_upgrade_allowed);
    }
}

fn insert_upgrade_simple_analytics(telemetry: &mut UpgradeTelemetry, status: UpgradeStatus) {
    telemetry.status = Some(status);
    telemetry.applied = Some(false);
    telemetry.scheduled = Some(false);
    telemetry.update_available = Some(false);
}

fn insert_upgrade_error_analytics(telemetry: &mut UpgradeTelemetry, error: &anyhow::Error) {
    telemetry.status = Some(UpgradeStatus::Failed);
    telemetry.applied = Some(false);
    telemetry.scheduled = Some(false);
    telemetry.failure_kind = Some(upgrade_failure_kind(error));
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
    create_private_directory_all(data_root)
        .with_context(|| format!("create private upgrade data root {}", data_root.display()))?;
    verify_private_directory(data_root)
        .with_context(|| format!("verify private upgrade data root {}", data_root.display()))
}

fn semantic_accelerator(platform: &str) -> Result<Option<SemanticAccelerator>> {
    let accelerator = semantic_native_accelerator_target().map(|accelerator| match accelerator {
        SemanticNativeAcceleratorTarget::CoreMl => SemanticAccelerator::CoreMl,
        SemanticNativeAcceleratorTarget::WindowsMl => SemanticAccelerator::WindowsMl,
        SemanticNativeAcceleratorTarget::Cuda => SemanticAccelerator::OrtCuda,
    });
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

fn check_upgrade(
    data_root: &Path,
    config: &AppConfig,
    channel_override: Option<&str>,
    command: &'static str,
) -> Result<UpgradeOutcome> {
    if let Some(recovery) = pending_recovery(data_root)? {
        if let Some(terminal) = recovery.terminal.as_ref() {
            let lock = UpgradeLock::acquire_terminal_recovery(&recovery)?;
            let (applied, detail) = match terminal {
                TerminalRecovery::Applied { warning } => (true, warning.as_deref()),
                TerminalRecovery::Failed { error } => (false, Some(error.as_str())),
            };
            reconcile_replacement_terminal_locked(
                &lock,
                &recovery.attempt_id,
                applied,
                detail,
                config.upgrade.interval,
            )?;
            remove_terminal_recovery(&recovery, lock.installation())?;
        } else {
            #[cfg(windows)]
            return Err(anyhow!(
                "interrupted Windows installation requires `ctx upgrade` so daemon handoff and replacement recovery remain coordinated"
            ));
            #[cfg(not(windows))]
            {
                let recovery_lock = UpgradeLock::acquire_recovery(&recovery)?;
                begin_recovery_attempt_locked(
                    &recovery_lock,
                    &recovery.attempt_id,
                    "manual_recovery",
                )?;
                let handoff = crate::semantic::begin_daemon_upgrade_handoff(
                    &recovery.data_root,
                    &recovery.attempt_id,
                )?;
                match recover_interrupted_install(&recovery, recovery_lock.installation())? {
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
                            (!committed).then_some("interrupted ctx installation was rolled back"),
                            config.upgrade.interval,
                        )?;
                        drop(recovery_lock);
                        handoff.resume_with(&current_install_path()?)?;
                    }
                    InstallRecovery::Scheduled { .. } => {
                        return Err(anyhow!("interrupted install recovery is still active"));
                    }
                    InstallRecovery::ReexecRequired(path) => {
                        reconcile_replacement_terminal_locked(
                            &recovery_lock,
                            &recovery.attempt_id,
                            false,
                            Some("interrupted ctx installation was rolled back"),
                            config.upgrade.interval,
                        )?;
                        drop(recovery_lock);
                        if recovery.legacy_v025() {
                            let _ = handoff.resume_legacy_reexec_with(&path)?;
                        } else {
                            handoff.prepare_reexec()?;
                        }
                        reexec_recovered_executable(&path, &recovery.attempt_id)?;
                        unreachable!("successful recovery re-exec does not return");
                    }
                }
            }
        }
    }
    let lock = UpgradeLock::acquire(data_root)?;
    let attempt = begin_manual_attempt_locked(data_root, &lock, command)?;
    let plan = match build_upgrade_plan(&lock, config, channel_override, false) {
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
    write_state_checked_locked(
        data_root,
        &lock,
        &attempt,
        &plan,
        status,
        config.upgrade.interval,
    )?;
    let message = if plan.update_available {
        format!(
            "ctx {} is available (current {}, channel {}).",
            plan.latest_version, plan.current_version, plan.channel
        )
    } else {
        format!("ctx {} is up to date.", plan.current_version)
    };
    let warnings = plan.warnings.clone();
    Ok(UpgradeOutcome {
        command,
        status,
        message,
        plan: Some(plan),
        applied: false,
        dry_run: false,
        warnings,
        attempt_id: Some(attempt.id().to_owned()),
    })
}

fn apply_upgrade(
    data_root: &Path,
    config: &AppConfig,
    channel_override: Option<&str>,
    dry_run: bool,
) -> Result<UpgradeOutcome> {
    if let Some(recovery) = pending_recovery(data_root)? {
        if let Some(terminal) = recovery.terminal.as_ref() {
            let lock = UpgradeLock::acquire_terminal_recovery(&recovery)?;
            let (applied, detail) = match terminal {
                TerminalRecovery::Applied { warning } => (true, warning.as_deref()),
                TerminalRecovery::Failed { error } => (false, Some(error.as_str())),
            };
            reconcile_replacement_terminal_locked(
                &lock,
                &recovery.attempt_id,
                applied,
                detail,
                config.upgrade.interval,
            )?;
            remove_terminal_recovery(&recovery, lock.installation())?;
            drop(lock);
        } else {
            let recovery_attempt_id = recovery.attempt_id.clone();
            let origin_root = recovery.data_root.clone();
            let recovery_lock = UpgradeLock::acquire_recovery(&recovery)?;
            begin_recovery_attempt_locked(&recovery_lock, &recovery_attempt_id, "manual_recovery")?;
            let daemon_handoff =
                crate::semantic::begin_daemon_upgrade_handoff(&origin_root, &recovery_attempt_id)?;
            match recover_interrupted_install(&recovery, recovery_lock.installation())? {
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
                        (!committed).then_some("interrupted ctx installation was rolled back"),
                        config.upgrade.interval,
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
                    if cfg!(debug_assertions)
                        && env_flag("CTX_UPGRADE_STOP_AFTER_RECOVERY_FOR_TESTS")
                    {
                        return Err(anyhow!("stopped after interrupted install recovery"));
                    }
                }
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
                InstallRecovery::ReexecRequired(path) => {
                    reconcile_replacement_terminal_locked(
                        &recovery_lock,
                        &recovery_attempt_id,
                        false,
                        Some("interrupted ctx installation was rolled back"),
                        config.upgrade.interval,
                    )?;
                    drop(recovery_lock);
                    if recovery.legacy_v025() {
                        // v0.25 cannot consume the v0.26 deferred-restart request
                        // contract. Restart and prove daemon readiness while this
                        // process still understands the request, then re-exec.
                        if let Some(warning) = daemon_handoff.resume_legacy_reexec_with(&path)? {
                            eprintln!("warning: {warning}");
                        }
                    } else {
                        daemon_handoff.prepare_reexec()?;
                    }
                    reexec_recovered_executable(&path, &recovery_attempt_id)?;
                    unreachable!("successful recovery re-exec does not return");
                }
            }
        }
    }
    if env::var(RECOVERY_REEXEC_ENV)
        .ok()
        .is_some_and(|attempt_id| is_valid_upgrade_attempt_id(&attempt_id))
    {
        env::remove_var(RECOVERY_REEXEC_ENV);
    }
    let upgrade_lock = UpgradeLock::acquire(data_root)?;
    let attempt = begin_manual_attempt_locked(data_root, &upgrade_lock, "manual_apply")?;
    let result = (|| -> Result<UpgradeOutcome> {
        let plan = build_upgrade_plan(&upgrade_lock, config, channel_override, true)?;
        let semantic_repair_required = semantic_install_required(&plan, data_root)?;
        if !plan.update_available && !semantic_repair_required {
            write_state_checked_locked(
                data_root,
                &upgrade_lock,
                &attempt,
                &plan,
                "up_to_date",
                config.upgrade.interval,
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
                config.upgrade.interval,
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
        let mut runtime_artifact = if plan.update_available && plan.semantic_provisioning.is_none()
        {
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
                (None, None) => None,
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
        write_state_phase_locked(&upgrade_lock, &attempt, "quiescing")?;
        let daemon_handoff =
            crate::semantic::begin_daemon_upgrade_handoff(data_root, attempt.id())?;
        let daemon_restart = daemon_handoff.replacement_restart();
        let mut before_publish = || Ok(());
        let apply_result = match apply_artifact(
            upgrade_lock.installation(),
            &plan,
            artifact.as_mut(),
            runtime_artifact.as_mut(),
            &mut semantic_artifacts,
            data_root,
            attempt.id(),
            daemon_restart,
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
                config.upgrade.interval,
                &mut warnings,
            );
            let message = if plan.update_available {
                format!(
                    "scheduled ctx {} -> {} at {}; replacement will finish after this process exits",
                    plan.current_version,
                    plan.latest_version,
                    plan.install_path.display()
                )
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
            config.upgrade.interval,
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

fn build_upgrade_plan(
    lock: &UpgradeLock,
    config: &AppConfig,
    channel_override: Option<&str>,
    require_managed: bool,
) -> Result<UpgradePlan> {
    let fallback_current_version = env!("CARGO_PKG_VERSION").to_owned();
    let platform = platform_key()?.to_owned();
    let channel = channel_override
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(config.upgrade.channel.as_str())
        .to_owned();
    let mut warnings = Vec::new();
    let snapshot = capture_install_snapshot(
        lock.installation(),
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
    let path = path_diagnostics(&snapshot.marker.install_path, &current_version);
    warnings.extend(path.warnings.clone());
    let metadata_url = metadata_url(config, &channel);
    let signature_url = metadata_signature_url(&metadata_url);
    let metadata_bytes = net::get_bytes_limited(&metadata_url, RELEASE_METADATA_MAX_BYTES)
        .with_context(|| format!("download release metadata {metadata_url}"))?;
    let signature_bytes =
        net::get_bytes_limited(&signature_url, RELEASE_METADATA_SIGNATURE_MAX_BYTES)
            .with_context(|| format!("download release metadata signature {signature_url}"))?;
    verify_metadata_signature(&metadata_bytes, &signature_bytes)?;
    let semantic_enabled = config.semantic_search_enabled();
    let metadata = parse_release_metadata(&metadata_bytes, &platform, &channel, semantic_enabled)?;
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
        semantic_accelerator(&platform)?
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
        path,
        metadata,
        semantic_provisioning,
    })
}

fn render_outcome(outcome: &UpgradeOutcome, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(&outcome.json())?);
    } else {
        println!("{}", outcome.message);
        for warning in &outcome.warnings {
            eprintln!("warning: {warning}");
        }
    }
    Ok(())
}
