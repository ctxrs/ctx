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
    resume_pending_managed_pair_under_installation_lock, ManagedPairApplyInput,
    ManagedPairApplyOutcome, ManagedPairComponentIdentity, ManagedPairInstallationStatus,
    ManagedPairTarget, ManagedPairVerifier, VerifiedManagedPairIdentity,
};
#[cfg(any(windows, test))]
use ctx_managed_pair_engine::{
    stage_managed_pair_under_installation_lock, ManagedPairStageOutcome,
};

#[cfg(any(windows, test))]
use super::state::validate_managed_pair_helper_file;
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
#[cfg(windows)]
use super::{
    state::{
        acquire_managed_pair_helper_recovery_lock, managed_pair_helper_recovery_hint,
        update_managed_pair_helper_parent_locked, ManagedPairRecovery,
    },
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
            reconcile_replacement_terminal_locked(
                &lock,
                attempt_id,
                true,
                None,
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
            Ok(Some(()))
        }
        Ok(false) => Err(anyhow!(
            "pending managed-pair recovery disappeared before publication"
        )),
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
    _installation_lock: &InstallationLock,
) -> Result<ManagedPairMode> {
    let Some(install_root) = install_root_for_executable(&plan.install_path) else {
        return Ok(ManagedPairMode::CoreOnly);
    };
    let verifier = ReleaseManagedPairVerifier::for_channel(&plan.channel)?;
    let status = inspect_managed_pair_under_installation_lock(&install_root, &verifier)?;
    Ok(match status {
        ManagedPairInstallationStatus::Absent => ManagedPairMode::CoreOnly,
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
        let identity = verify_signed_managed_pair_envelope(&self.expectations, signed_envelope)
            .map_err(|error| anyhow!(error.to_string()))?;
        engine_identity(&identity)
    }
}

/// The four retained inputs shared by foreground and automatic pair apply.
pub(super) struct ManagedPairDownloads {
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
        apply_or_resume_managed_pair_under_installation_lock(install_root, &input, verifier)
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

fn install_root_for_executable(install_path: &Path) -> Option<PathBuf> {
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
mod tests {
    use std::{collections::BTreeMap, fs, io::Write as _, sync::Mutex, time::Duration};

    use ctx_history_platform::platform_security::restrict_private_directory;
    use serde_json::json;
    use sha2::{Digest as _, Sha256};
    use tempfile::tempdir;

    use super::*;
    use crate::upgrade::{
        install::{install_marker_path, InstallFingerprint},
        metadata::{ManagedPairReleaseMetadata, ReleaseMetadata},
    };

    struct FixtureVerifier(VerifiedManagedPairIdentity);

    impl ManagedPairVerifier for FixtureVerifier {
        fn verify_signed_envelope(
            &self,
            signed_envelope: &[u8],
        ) -> Result<VerifiedManagedPairIdentity> {
            if signed_envelope != b"signed-envelope" {
                bail!("unexpected envelope")
            }
            Ok(self.0.clone())
        }
    }

    struct FixtureTransport {
        bytes: BTreeMap<String, Vec<u8>>,
        downloads: Mutex<Vec<String>>,
    }

    impl ReleaseTransport for FixtureTransport {
        fn get_bytes_limited(&self, endpoint: &str, max_bytes: usize) -> Result<Vec<u8>> {
            let bytes = self
                .bytes
                .get(endpoint)
                .ok_or_else(|| anyhow!("unexpected endpoint {endpoint}"))?
                .clone();
            if bytes.len() > max_bytes {
                bail!("fixture response exceeds bound")
            }
            Ok(bytes)
        }

        fn download_artifact(
            &self,
            endpoint: &str,
            destination: &mut fs::File,
            max_bytes: u64,
            _timeout: Duration,
        ) -> Result<u64> {
            let bytes = self
                .bytes
                .get(endpoint)
                .ok_or_else(|| anyhow!("unexpected endpoint {endpoint}"))?;
            if bytes.len() as u64 > max_bytes {
                bail!("fixture artifact exceeds bound")
            }
            self.downloads.lock().unwrap().push(endpoint.to_owned());
            destination.write_all(bytes)?;
            Ok(bytes.len() as u64)
        }
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn current_target() -> ManagedPairTarget {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "aarch64") => ManagedPairTarget::LinuxArm64,
            ("linux", "x86_64") => ManagedPairTarget::LinuxX64,
            ("macos", "aarch64") => ManagedPairTarget::MacosArm64,
            ("macos", "x86_64") => ManagedPairTarget::MacosX64,
            ("windows", "x86_64") => ManagedPairTarget::WindowsX64,
            pair => panic!("unsupported test target {pair:?}"),
        }
    }

    #[test]
    fn one_pair_fixture_drives_download_marker_and_apply_without_legacy_core() -> Result<()> {
        let release_verifier = ReleaseManagedPairVerifier::for_channel("stable")?;
        assert!(release_verifier
            .verify_signed_envelope(b"not-a-signed-envelope")
            .is_err());

        let fixture = tempdir()?;
        restrict_private_directory(fixture.path())?;
        let bin = fixture.path().join("bin");
        fs::create_dir(&bin)?;
        restrict_private_directory(&bin)?;
        let core_path = bin.join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
        fs::write(&core_path, b"old-core")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&core_path, fs::Permissions::from_mode(0o700))?;
        }
        validate_managed_pair_helper_file(&core_path, &digest(b"old-core"))?;
        assert!(validate_managed_pair_helper_file(&core_path, &"0".repeat(64)).is_err());
        let marker_path = install_marker_path(&core_path);
        fs::write(
            &marker_path,
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "manager": "ctx-hosted-installer",
                "install_path": core_path,
                "platform": super::super::platform_key()?,
                "channel": "stable",
                "version": "1.0.0",
                "sha256": digest(b"old-core"),
                "installed_at": "2026-09-02T00:00:00Z",
                "man_pages": {"status": "installed"},
            }))?,
        )?;

        let next_core = b"next-core";
        let companion = b"next-companion";
        let core_sha = digest(next_core);
        let companion_sha = digest(companion);
        let plan = UpgradePlan {
            current_version: "1.0.0".to_owned(),
            latest_version: "1.1.0".to_owned(),
            channel: "stable".to_owned(),
            platform: super::super::platform_key()?.to_owned(),
            metadata_url: "metadata".to_owned(),
            artifact_url: "legacy-core".to_owned(),
            artifact_sha256: core_sha.clone(),
            install_path: core_path.clone(),
            install_fingerprint: InstallFingerprint {
                binary_sha256: digest(b"old-core"),
                marker_sha256: digest(&fs::read(&marker_path)?),
            },
            update_available: true,
            managed: true,
            warnings: Vec::new(),
            managed_pair_release: Some(ManagedPairReleaseMetadata {
                envelope_url: "pair-envelope".to_owned(),
                core_object_url: "pair-core".to_owned(),
                core_sha256: core_sha.clone(),
                companion_object_url: "pair-companion".to_owned(),
                companion_sha256: companion_sha.clone(),
            }),
            metadata: ReleaseMetadata {
                version: "1.1.0".to_owned(),
                base_url: "https://cli.ctx.rs/releases/1.1.0".to_owned(),
                artifact: "ctx".to_owned(),
                sha256: core_sha.clone(),
                source_commit: None,
                published_at: None,
                self_upgrade_allowed: true,
                auto_upgrade_allowed: true,
                store_schema_version: None,
                managed_pair: None,
                onnxruntime: None,
                semantic: None,
            },
            semantic_provisioning: None,
        };
        let verifier = FixtureVerifier(VerifiedManagedPairIdentity::new(
            "ctx-1.1.0",
            current_target(),
            2,
            "3".repeat(64),
            ManagedPairComponentIdentity::new(&core_sha, next_core.len() as u64)?,
            ManagedPairComponentIdentity::new(&companion_sha, companion.len() as u64)?,
        )?);
        let mut staging_plan = plan.clone();
        staging_plan.channel = "staging".to_owned();
        let staging_marker: serde_json::Value = serde_json::from_slice(
            &install::install_marker_bytes(&marker_path, &staging_plan, None)?,
        )?;
        assert_eq!(staging_marker["staging_dogfood"], true);
        let transport = FixtureTransport {
            bytes: BTreeMap::from([
                ("pair-envelope".to_owned(), b"signed-envelope".to_vec()),
                ("pair-core".to_owned(), next_core.to_vec()),
                ("pair-companion".to_owned(), companion.to_vec()),
                ("legacy-core".to_owned(), b"must-not-download".to_vec()),
            ]),
            downloads: Mutex::new(Vec::new()),
        };
        let paired_update = ManagedPairMode::Paired {
            install_root: fixture.path().to_path_buf(),
            repair_required: false,
        };
        assert_eq!(
            core_download_route(&plan, &paired_update),
            CoreDownloadRoute::ManagedPair
        );
        assert_eq!(
            core_download_route(&plan, &ManagedPairMode::CoreOnly),
            CoreDownloadRoute::Legacy
        );

        let mut downloads =
            ManagedPairDownloads::download(&transport, fixture.path(), &plan, &verifier)?;
        assert_eq!(
            transport.downloads.lock().unwrap().as_slice(),
            ["pair-core", "pair-companion"]
        );
        assert!(!transport
            .downloads
            .lock()
            .unwrap()
            .iter()
            .any(|endpoint| endpoint == "legacy-core"));
        let lock = InstallationLock::try_acquire_at_root(fixture.path())?.expect("pair lock");
        let outcome = downloads.apply_under_installation_lock(&lock, fixture.path(), &verifier)?;
        assert!(matches!(outcome, ManagedPairApplyOutcome::Applied { .. }));
        assert_eq!(fs::read(&core_path)?, next_core);
        assert!(resume_or_confirm_pending_with_verifier(
            &core_path,
            &core_sha,
            &digest(b"signed-envelope"),
            &lock,
            &verifier,
        )?);
        assert!(!resume_or_confirm_pending_with_verifier(
            &core_path,
            &core_sha,
            &digest(b"signed-envelope-with-new-companion"),
            &lock,
            &verifier,
        )?);
        let companion_name = if cfg!(windows) {
            "ctx-pro.exe"
        } else {
            "ctx-pro"
        };
        assert_eq!(
            fs::read(fixture.path().join("libexec").join(companion_name))?,
            companion
        );
        let marker: serde_json::Value = serde_json::from_slice(&fs::read(&marker_path)?)?;
        assert_eq!(marker["version"], "1.1.0");
        assert_eq!(marker["man_pages"]["status"], "installed");
        assert!(fixture
            .path()
            .join("share/ctx/managed-pair-state.json")
            .is_file());
        drop(lock);

        fs::write(
            fixture.path().join("libexec").join(companion_name),
            b"damaged",
        )?;
        let mut repair_plan = plan.clone();
        repair_plan.current_version = "1.1.0".to_owned();
        repair_plan.latest_version = "1.1.0".to_owned();
        repair_plan.update_available = false;
        repair_plan.install_fingerprint = InstallFingerprint {
            binary_sha256: digest(next_core),
            marker_sha256: digest(&fs::read(&marker_path)?),
        };
        let lock = InstallationLock::try_acquire_at_root(fixture.path())?.expect("repair lock");
        let repair_mode = inspect_plan_under_installation_lock(&repair_plan, &lock)?;
        assert!(repair_mode.pair_apply_required(&repair_plan));
        assert_eq!(
            core_download_route(&repair_plan, &repair_mode),
            CoreDownloadRoute::ManagedPair
        );
        let mut repair =
            ManagedPairDownloads::download(&transport, fixture.path(), &repair_plan, &verifier)?;
        let staged = stage_managed_pair_under_installation_lock(
            fixture.path(),
            &repair.input()?,
            &verifier,
        )?;
        assert!(matches!(staged, ManagedPairStageOutcome::Staged { .. }));
        assert!(!resume_or_confirm_pending_with_verifier(
            &core_path,
            &core_sha,
            &digest(b"signed-envelope-with-new-companion"),
            &lock,
            &verifier,
        )?);
        assert_eq!(
            fs::read(fixture.path().join("libexec").join(companion_name))?,
            companion
        );
        Ok(())
    }
}
