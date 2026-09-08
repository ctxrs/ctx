use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, bail, Context as _, Result};
use ctx_companion_bridge::{
    verify_signed_managed_pair_envelope, ManagedPairExpectations, ReleaseChannel,
    SignedManagedPairIdentity, SignedManagedPairTarget,
};
use ctx_managed_pair_engine::{
    apply_or_resume_managed_pair_under_installation_lock,
    inspect_managed_pair_under_installation_lock,
    preflight_pending_managed_pair_under_installation_lock,
    resume_pending_managed_pair_under_installation_lock, ManagedPairApplyInput,
    ManagedPairApplyOutcome, ManagedPairComponentIdentity, ManagedPairInstallationStatus,
    ManagedPairTarget, ManagedPairVerifier, VerifiedManagedPairIdentity,
};
#[cfg(any(windows, test))]
use ctx_managed_pair_engine::{
    stage_managed_pair_under_installation_lock, ManagedPairStageOutcome,
};

#[cfg(windows)]
use super::state::{
    acquire_managed_pair_helper_recovery_lock, managed_pair_helper_recovery_hint,
    update_managed_pair_helper_parent_locked,
};
use super::{
    command::RELEASE_ARTIFACT_TIMEOUT,
    download::DownloadedArtifact,
    install::{self, ApplyResult, InstallationLock},
    state::{
        acquire_managed_pair_recovery_lock, begin_recovery_attempt_locked,
        managed_pair_recovery_hint, managed_pair_recovery_locked,
        reconcile_replacement_terminal_locked, write_managed_pair_attempt_locked, UpgradeAttempt,
        UpgradeLock,
    },
    DaemonUpgradeLease, DaemonUpgradePort, ReleaseProcessPort, ReleaseTransport,
    SemanticLayoutPort, UpgradeEngine, UpgradePlan,
};
#[cfg(any(windows, test))]
use super::{
    state::{validate_managed_pair_helper_file, ManagedPairRecovery},
    DaemonRestart,
};

const MANAGED_PAIR_ENVELOPE_MAX_BYTES: usize = 2 * 1024 * 1024;
const RELEASE_ARTIFACT_MAX_BYTES: u64 = 128 * 1024 * 1024;

pub(super) enum ForegroundManagedPairRecovery {
    None,
    Recovered,
    #[cfg(windows)]
    Scheduled {
        attempt_id: String,
        helper_pid: u32,
    },
}

pub(super) fn recover_foreground_before_generic<D: DaemonUpgradePort + ?Sized>(
    engine: &UpgradeEngine<'_, D>,
    check_only: bool,
) -> Result<ForegroundManagedPairRecovery> {
    let Some(attempt_id) = managed_pair_recovery_hint()? else {
        return Ok(ForegroundManagedPairRecovery::None);
    };
    #[cfg(windows)]
    if check_only {
        return Err(anyhow!(
            "interrupted Windows managed-pair installation requires `ctx upgrade` so daemon handoff and post-exit recovery remain coordinated"
        ));
    }
    #[cfg(not(windows))]
    let _ = check_only;

    let lock = acquire_managed_pair_recovery_lock(&attempt_id)?;
    let recovery = managed_pair_recovery_locked(&lock, &attempt_id)?;
    preflight_recovery(&recovery, lock.installation())?;
    let attempt = begin_recovery_attempt_locked(&lock, &attempt_id, "manual_recovery")?;
    #[cfg(not(windows))]
    let _ = &attempt;
    let handoff = engine.daemon.begin(&recovery.data_root, &attempt_id)?;

    #[cfg(windows)]
    {
        let helper_pid =
            schedule_existing_windows_helper(engine.process, &recovery, &lock, &attempt)?;
        handoff.transfer_to_replacement_helper(helper_pid)?;
        drop(lock);
        Ok(ForegroundManagedPairRecovery::Scheduled {
            attempt_id,
            helper_pid,
        })
    }
    #[cfg(not(windows))]
    {
        let recovered = resume_or_confirm_pending_under_installation_lock(
            &recovery.install_path,
            &recovery.channel,
            &recovery.core_sha256,
            &recovery.envelope_sha256,
            lock.installation(),
        );
        let recovered = match recovered {
            Ok(recovered) => recovered,
            Err(error) => {
                drop(lock);
                return match handoff.resume_with(&recovery.install_path) {
                    Ok(()) => Err(error),
                    Err(restart_error) => Err(error.context(format!(
                        "also failed to resume daemon lifecycle after managed-pair recovery failure: {restart_error:#}"
                    ))),
                };
            }
        };
        reconcile_replacement_terminal_locked(
            &lock,
            &attempt_id,
            recovered,
            (!recovered).then_some("managed-pair recovery record disappeared before publication"),
            recovery.interval,
        )?;
        drop(lock);
        handoff.resume_with(&recovery.install_path)?;
        Ok(ForegroundManagedPairRecovery::Recovered)
    }
}

#[cfg(windows)]
pub(super) fn schedule_existing_windows_helper(
    process: &dyn ReleaseProcessPort,
    recovery: &ManagedPairRecovery,
    lock: &UpgradeLock,
    attempt: &UpgradeAttempt,
) -> Result<u32> {
    let helper_path = recovery
        .helper_path
        .as_deref()
        .ok_or_else(|| anyhow!("pending Windows managed-pair upgrade has no retained helper"))?;
    update_managed_pair_helper_parent_locked(lock, attempt)?;
    validate_managed_pair_helper_file(helper_path, &recovery.core_sha256)?;
    install::spawn_managed_pair_helper(
        process,
        helper_path,
        &recovery.data_root,
        &recovery.install_path,
        &recovery.attempt_id,
        std::process::id(),
    )
}

#[cfg(windows)]
pub(super) fn run_windows_helper<D: DaemonUpgradePort + ?Sized>(
    daemon: &D,
    install_path: &Path,
    attempt_id: &str,
    parent_pid: u32,
) -> Result<Option<()>> {
    let Some(initial) = managed_pair_helper_recovery_hint(install_path, attempt_id, parent_pid)?
    else {
        return Ok(None);
    };
    let parent = install::open_managed_pair_parent(parent_pid)?;
    let helper_pid = std::process::id();
    daemon.mark_replacement_helper_handoff(&initial.data_root, attempt_id, helper_pid)?;
    install::write_managed_pair_helper_ready(attempt_id, helper_pid)?;

    let lock = acquire_managed_pair_helper_recovery_lock(install_path, attempt_id)?;
    let recovery = managed_pair_recovery_locked(&lock, attempt_id)?;
    parent.wait()?;
    let publication = resume_or_confirm_pending_under_installation_lock(
        &recovery.install_path,
        &recovery.channel,
        &recovery.core_sha256,
        &recovery.envelope_sha256,
        lock.installation(),
    );
    let restart = recovery
        .restart_trigger
        .as_deref()
        .map(|trigger| DaemonRestart {
            trigger,
            loop_interval_seconds: recovery.restart_interval_seconds,
        });
    match publication {
        Ok(true) => {
            finish_windows_managed_pair_helper_recovery(
                daemon, &recovery, lock, attempt_id, restart, true, None,
            )?;
            Ok(Some(()))
        }
        Ok(false) => {
            let error = anyhow!("pending managed-pair recovery disappeared before publication");
            finish_windows_managed_pair_helper_recovery(
                daemon,
                &recovery,
                lock,
                attempt_id,
                restart,
                false,
                Some("managed-pair recovery record disappeared before publication"),
            )?;
            Err(error)
        }
        Err(error) => {
            let restart_error = daemon
                .complete_replacement_handoff(
                    &recovery.data_root,
                    &recovery.install_path,
                    attempt_id,
                    restart,
                )
                .err();
            drop(lock);
            match restart_error {
                Some(restart_error) => Err(error.context(format!(
                    "also failed to resume daemon lifecycle after managed-pair helper failure: {restart_error:#}"
                ))),
                None => Err(error),
            }
        }
    }
}

#[cfg(any(windows, test))]
fn finish_windows_managed_pair_helper_recovery<D: DaemonUpgradePort + ?Sized>(
    daemon: &D,
    recovery: &ManagedPairRecovery,
    lock: UpgradeLock,
    attempt_id: &str,
    restart: Option<DaemonRestart<'_>>,
    applied: bool,
    warning_or_error: Option<&str>,
) -> Result<()> {
    reconcile_replacement_terminal_locked(
        &lock,
        attempt_id,
        applied,
        warning_or_error,
        recovery.interval,
    )?;
    daemon.complete_replacement_handoff(
        &recovery.data_root,
        &recovery.install_path,
        attempt_id,
        restart,
    )?;
    drop(lock);
    let _ = daemon.finish_replacement_handoff(&recovery.data_root, attempt_id);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManagedPairMode {
    CoreOnly,
    Paired {
        install_root: PathBuf,
        repair_required: bool,
    },
}

impl ManagedPairMode {
    pub(super) fn pair_apply_required(&self, plan: &UpgradePlan) -> bool {
        match self {
            Self::CoreOnly => false,
            Self::Paired {
                repair_required, ..
            } => plan.update_available || *repair_required,
        }
    }

    pub(super) fn install_root(&self) -> Option<&Path> {
        match self {
            Self::CoreOnly => None,
            Self::Paired { install_root, .. } => Some(install_root),
        }
    }
}

pub(super) struct ReleaseManagedPairVerifier {
    expectations: ManagedPairExpectations,
}

impl ReleaseManagedPairVerifier {
    pub(super) fn for_channel(channel: &str) -> Result<Self> {
        let channel = match channel {
            "stable" => ReleaseChannel::Stable,
            "staging" => ReleaseChannel::Staging,
            other => bail!("managed-pair upgrades do not support release channel {other}"),
        };
        Ok(Self {
            expectations: ManagedPairExpectations::new(channel),
        })
    }
}

pub(super) fn inspect_plan_under_installation_lock(
    plan: &UpgradePlan,
    installation_lock: &InstallationLock,
) -> Result<ManagedPairMode> {
    let Some(install_root) = install_root_for_executable(&plan.install_path) else {
        return Ok(ManagedPairMode::CoreOnly);
    };
    let verifier = ReleaseManagedPairVerifier::for_channel(&plan.channel)?;
    let status = inspect_managed_pair_under_installation_lock(&install_root, &verifier)?;
    Ok(match status {
        ManagedPairInstallationStatus::Absent => {
            // Hosted installations predating the paired distribution have no
            // pair evidence yet. Complete their first pair through the same
            // verified download, daemon handoff and publication owner as repair.
            // A source/unmanaged plan or a Core-only release grants no such work.
            if !plan.managed || plan.managed_pair_release.is_none() {
                return Ok(ManagedPairMode::CoreOnly);
            }
            install::revalidate_plan_snapshot_under_installation_lock(plan, installation_lock)?;
            let current = super::version::parse_semver(&plan.current_version)?;
            let latest = super::version::parse_semver(&plan.latest_version)?;
            if !latest.cmp_precedence(&current).is_gt()
                && plan.latest_version != plan.current_version
            {
                bail!("managed-pair completion requires a newer release or the exact installed Core version");
            }
            if !plan.metadata.self_upgrade_allowed {
                bail!(
                    "release {} does not allow self-upgrade",
                    plan.latest_version
                );
            }
            ManagedPairMode::Paired {
                install_root,
                repair_required: true,
            }
        }
        ManagedPairInstallationStatus::Healthy { .. } => ManagedPairMode::Paired {
            install_root,
            repair_required: false,
        },
        ManagedPairInstallationStatus::RepairRequired => ManagedPairMode::Paired {
            install_root,
            repair_required: true,
        },
    })
}

pub(super) fn preflight_recovery(
    recovery: &super::state::ManagedPairRecovery,
    _installation_lock: &InstallationLock,
) -> Result<()> {
    let Some(root) = install_root_for_executable(&recovery.install_path) else {
        return Ok(());
    };
    let verifier = ReleaseManagedPairVerifier::for_channel(&recovery.channel)?;
    preflight_pending_managed_pair_under_installation_lock(
        &root,
        &recovery.core_sha256,
        &recovery.envelope_sha256,
        &verifier,
    )
}

pub(super) fn resume_or_confirm_pending_under_installation_lock(
    install_path: &Path,
    channel: &str,
    expected_core_sha256: &str,
    expected_envelope_sha256: &str,
    installation_lock: &InstallationLock,
) -> Result<bool> {
    let verifier = ReleaseManagedPairVerifier::for_channel(channel)?;
    resume_or_confirm_pending_with_verifier(
        install_path,
        expected_core_sha256,
        expected_envelope_sha256,
        installation_lock,
        &verifier,
    )
}

fn resume_or_confirm_pending_with_verifier(
    install_path: &Path,
    expected_core_sha256: &str,
    expected_envelope_sha256: &str,
    _installation_lock: &InstallationLock,
    verifier: &dyn ManagedPairVerifier,
) -> Result<bool> {
    let Some(install_root) = install_root_for_executable(install_path) else {
        return Ok(false);
    };
    preflight_pending_managed_pair_under_installation_lock(
        &install_root,
        expected_core_sha256,
        expected_envelope_sha256,
        verifier,
    )?;
    let _ = resume_pending_managed_pair_under_installation_lock(&install_root, verifier)?;
    Ok(matches!(
        inspect_managed_pair_under_installation_lock(&install_root, verifier)?,
        ManagedPairInstallationStatus::Healthy { identity, envelope_sha256 }
            if identity.core().sha256().eq_ignore_ascii_case(expected_core_sha256)
                && envelope_sha256.eq_ignore_ascii_case(expected_envelope_sha256)
    ))
}

impl ManagedPairVerifier for ReleaseManagedPairVerifier {
    fn verify_signed_envelope(
        &self,
        signed_envelope: &[u8],
    ) -> Result<VerifiedManagedPairIdentity> {
        #[cfg(test)]
        if let Some(result) = tests::verify_fixture(self.expectations.channel(), signed_envelope) {
            return result;
        }
        let identity = verify_signed_managed_pair_envelope(&self.expectations, signed_envelope)
            .map_err(|error| anyhow!(error.to_string()))?;
        engine_identity(&identity)
    }
}

/// The four retained inputs shared by foreground and automatic pair apply.
pub(super) struct ManagedPairDownloads {
    identity: VerifiedManagedPairIdentity,
    envelope: DownloadedArtifact,
    core: DownloadedArtifact,
    companion: DownloadedArtifact,
    marker: DownloadedArtifact,
}

impl ManagedPairDownloads {
    fn envelope_sha256(&self) -> &str {
        self.envelope.sha256()
    }

    pub(super) fn download(
        transport: &dyn ReleaseTransport,
        managed_root: &Path,
        plan: &UpgradePlan,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<Self> {
        let release = plan
            .managed_pair_release
            .as_ref()
            .ok_or_else(|| anyhow!("signed release metadata has no managed-pair candidate"))?;
        let envelope_bytes = transport
            .get_bytes_limited(&release.envelope_url, MANAGED_PAIR_ENVELOPE_MAX_BYTES)
            .with_context(|| format!("download managed-pair envelope {}", release.envelope_url))?;
        let identity = verifier
            .verify_signed_envelope(&envelope_bytes)
            .context("verify downloaded managed-pair envelope")?;
        validate_release_identity(plan, &identity)?;

        let envelope = DownloadedArtifact::from_bytes(
            managed_root,
            &envelope_bytes,
            MANAGED_PAIR_ENVELOPE_MAX_BYTES as u64,
            "managed-pair signed envelope",
        )?;
        let core = DownloadedArtifact::download_or_reuse_verified(
            transport,
            managed_root,
            &release.core_object_url,
            identity.core().sha256(),
            identity.core().size_bytes(),
            RELEASE_ARTIFACT_TIMEOUT,
        )
        .with_context(|| format!("download or reuse {}", release.core_object_url))?;
        let companion = DownloadedArtifact::download_or_reuse_verified(
            transport,
            managed_root,
            &release.companion_object_url,
            identity.companion().sha256(),
            identity.companion().size_bytes(),
            RELEASE_ARTIFACT_TIMEOUT,
        )
        .with_context(|| format!("download or reuse {}", release.companion_object_url))?;

        let current_marker = install::install_marker_path(&plan.install_path);
        let attribution = install::existing_install_attribution(&current_marker);
        let marker_bytes =
            install::install_marker_bytes(&current_marker, plan, attribution.as_ref())?;
        let marker = DownloadedArtifact::from_bytes(
            managed_root,
            &marker_bytes,
            install::MAX_INSTALL_MARKER_BYTES,
            "managed Core install marker",
        )?;
        Ok(Self {
            identity,
            envelope,
            core,
            companion,
            marker,
        })
    }

    pub(super) fn apply_under_installation_lock(
        &mut self,
        _installation_lock: &InstallationLock,
        install_root: &Path,
        verifier: &dyn ManagedPairVerifier,
    ) -> Result<ManagedPairApplyOutcome> {
        let input = ManagedPairApplyInput::new(
            self.envelope.retained_path()?.to_path_buf(),
            self.core.retained_path()?.to_path_buf(),
            self.companion.retained_path()?.to_path_buf(),
            self.marker.retained_path()?.to_path_buf(),
        );
        let outcome =
            apply_or_resume_managed_pair_under_installation_lock(install_root, &input, verifier)?;
        if outcome.identity() != &self.identity {
            bail!("managed-pair publication did not apply the requested signed candidate");
        }
        Ok(outcome)
    }

    pub(super) fn apply_plan_under_installation_lock(
        &mut self,
        plan: &UpgradePlan,
        mode: &ManagedPairMode,
        installation_lock: &InstallationLock,
    ) -> Result<ManagedPairApplyOutcome> {
        install::revalidate_plan_snapshot_under_installation_lock(plan, installation_lock)?;
        let install_root = mode
            .install_root()
            .ok_or_else(|| anyhow!("Core-only upgrade cannot apply a managed pair"))?;
        let verifier = ReleaseManagedPairVerifier::for_channel(&plan.channel)?;
        self.apply_under_installation_lock(installation_lock, install_root, &verifier)
    }

    #[cfg(windows)]
    pub(super) fn stage_plan_under_installation_lock(
        &mut self,
        plan: &UpgradePlan,
        mode: &ManagedPairMode,
        installation_lock: &InstallationLock,
    ) -> Result<ManagedPairStageOutcome> {
        install::revalidate_plan_snapshot_under_installation_lock(plan, installation_lock)?;
        let install_root = mode
            .install_root()
            .ok_or_else(|| anyhow!("Core-only upgrade cannot stage a managed pair"))?;
        let verifier = ReleaseManagedPairVerifier::for_channel(&plan.channel)?;
        let input = self.input()?;
        stage_managed_pair_under_installation_lock(install_root, &input, &verifier)
    }

    #[cfg(windows)]
    pub(super) fn retained_core_path(&mut self) -> Result<&Path> {
        self.core.retained_path()
    }

    #[cfg(any(windows, test))]
    fn input(&mut self) -> Result<ManagedPairApplyInput> {
        Ok(ManagedPairApplyInput::new(
            self.envelope.retained_path()?.to_path_buf(),
            self.core.retained_path()?.to_path_buf(),
            self.companion.retained_path()?.to_path_buf(),
            self.marker.retained_path()?.to_path_buf(),
        ))
    }
}

pub(super) enum PreparedCoreArtifact {
    None,
    Legacy(DownloadedArtifact),
    ManagedPair(Box<ManagedPairDownloads>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoreDownloadRoute {
    None,
    Legacy,
    ManagedPair,
}

fn core_download_route(plan: &UpgradePlan, pair_mode: &ManagedPairMode) -> CoreDownloadRoute {
    if pair_mode.pair_apply_required(plan) {
        CoreDownloadRoute::ManagedPair
    } else if plan.update_available {
        CoreDownloadRoute::Legacy
    } else {
        CoreDownloadRoute::None
    }
}

pub(super) fn download_core_artifact(
    transport: &dyn ReleaseTransport,
    data_root: &Path,
    plan: &UpgradePlan,
    pair_mode: &ManagedPairMode,
) -> Result<PreparedCoreArtifact> {
    match core_download_route(plan, pair_mode) {
        CoreDownloadRoute::ManagedPair => {
            let verifier = ReleaseManagedPairVerifier::for_channel(&plan.channel)?;
            ManagedPairDownloads::download(transport, data_root, plan, &verifier)
                .map(Box::new)
                .map(PreparedCoreArtifact::ManagedPair)
        }
        CoreDownloadRoute::Legacy => DownloadedArtifact::download_verified(
            transport,
            data_root,
            &plan.artifact_url,
            &plan.artifact_sha256,
            RELEASE_ARTIFACT_MAX_BYTES,
            RELEASE_ARTIFACT_TIMEOUT,
        )
        .with_context(|| format!("download {}", plan.artifact_url))
        .map(PreparedCoreArtifact::Legacy),
        CoreDownloadRoute::None => Ok(PreparedCoreArtifact::None),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_prepared_install(
    process: &dyn ReleaseProcessPort,
    semantic_layout: &dyn SemanticLayoutPort,
    upgrade_lock: &UpgradeLock,
    plan: &UpgradePlan,
    pair_mode: &ManagedPairMode,
    core: &mut PreparedCoreArtifact,
    runtime: Option<&mut DownloadedArtifact>,
    semantic: &mut [DownloadedArtifact],
    data_root: &Path,
    attempt: &UpgradeAttempt,
    interval: Duration,
    daemon_restart: Option<(&str, Option<u64>)>,
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    let installation_lock = upgrade_lock.installation();
    install::ensure_hosted_transaction_inactive_under_installation_lock(&plan.install_path)?;
    match core {
        PreparedCoreArtifact::Legacy(artifact) => install::apply_artifact(
            process,
            semantic_layout,
            installation_lock,
            plan,
            Some(artifact),
            runtime,
            semantic,
            data_root,
            attempt.id(),
            daemon_restart,
            before_publish,
        ),
        PreparedCoreArtifact::None => install::apply_artifact(
            process,
            semantic_layout,
            installation_lock,
            plan,
            None,
            runtime,
            semantic,
            data_root,
            attempt.id(),
            daemon_restart,
            before_publish,
        ),
        PreparedCoreArtifact::ManagedPair(downloads) => {
            if runtime.is_some() || !semantic.is_empty() {
                let result = install::apply_artifact(
                    process,
                    semantic_layout,
                    installation_lock,
                    plan,
                    None,
                    runtime,
                    semantic,
                    data_root,
                    attempt.id(),
                    daemon_restart,
                    before_publish,
                )?;
                if matches!(result, ApplyResult::Scheduled { .. }) {
                    return Ok(result);
                }
            } else {
                before_publish()?;
            }

            #[cfg(not(windows))]
            {
                write_managed_pair_attempt_locked(
                    data_root,
                    upgrade_lock,
                    attempt,
                    plan,
                    "applying",
                    interval,
                    daemon_restart,
                    downloads.envelope_sha256(),
                )?;
                downloads.apply_plan_under_installation_lock(plan, pair_mode, installation_lock)?;
                Ok(ApplyResult::Applied)
            }
            #[cfg(windows)]
            {
                let helper_path = install::prepare_managed_pair_helper(
                    downloads.retained_core_path()?,
                    &plan.install_path,
                    attempt.id(),
                )?;
                write_managed_pair_attempt_locked(
                    data_root,
                    upgrade_lock,
                    attempt,
                    plan,
                    "applying",
                    interval,
                    daemon_restart,
                    downloads.envelope_sha256(),
                    Some(&helper_path),
                )?;
                match downloads.stage_plan_under_installation_lock(
                    plan,
                    pair_mode,
                    installation_lock,
                )? {
                    ManagedPairStageOutcome::AlreadyCurrent { .. } => Ok(ApplyResult::Applied),
                    ManagedPairStageOutcome::Staged { .. } => {
                        let helper_pid = install::spawn_managed_pair_helper(
                            process,
                            &helper_path,
                            data_root,
                            &plan.install_path,
                            attempt.id(),
                            std::process::id(),
                        )?;
                        Ok(ApplyResult::Scheduled { helper_pid })
                    }
                }
            }
        }
    }
}

pub(super) fn install_root_for_executable(install_path: &Path) -> Option<PathBuf> {
    let bin = install_path
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "bin"))
        .filter(|_| {
            install_path
                .file_name()
                .is_some_and(|name| name == core_file_name())
        })?;
    bin.parent().map(Path::to_path_buf)
}

fn core_file_name() -> &'static str {
    if cfg!(windows) {
        "ctx.exe"
    } else {
        "ctx"
    }
}

fn validate_release_identity(
    plan: &UpgradePlan,
    identity: &VerifiedManagedPairIdentity,
) -> Result<()> {
    let release = plan
        .managed_pair_release
        .as_ref()
        .ok_or_else(|| anyhow!("signed release metadata has no managed-pair candidate"))?;
    if !release
        .core_sha256
        .eq_ignore_ascii_case(identity.core().sha256())
        || !release
            .companion_sha256
            .eq_ignore_ascii_case(identity.companion().sha256())
        || !plan
            .artifact_sha256
            .eq_ignore_ascii_case(&release.core_sha256)
    {
        bail!("signed release metadata does not match its managed-pair envelope");
    }
    Ok(())
}

fn engine_identity(identity: &SignedManagedPairIdentity) -> Result<VerifiedManagedPairIdentity> {
    let target = match identity.target() {
        SignedManagedPairTarget::LinuxArm64 => ManagedPairTarget::LinuxArm64,
        SignedManagedPairTarget::LinuxX64 => ManagedPairTarget::LinuxX64,
        SignedManagedPairTarget::MacosArm64 => ManagedPairTarget::MacosArm64,
        SignedManagedPairTarget::MacosX64 => ManagedPairTarget::MacosX64,
        SignedManagedPairTarget::WindowsX64 => ManagedPairTarget::WindowsX64,
    };
    VerifiedManagedPairIdentity::new(
        identity.release_name(),
        target,
        identity.rollback_generation(),
        identity.manifest_sha256().to_hex(),
        ManagedPairComponentIdentity::new(
            identity.core().sha256().to_hex(),
            identity.core().size_bytes(),
        )?,
        ManagedPairComponentIdentity::new(
            identity.companion().sha256().to_hex(),
            identity.companion().size_bytes(),
        )?,
    )
}

#[cfg(test)]
pub(super) mod tests;
