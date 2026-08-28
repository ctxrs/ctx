use std::{
    fs::File,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{anyhow, Result};
use ctx_upgrade_engine::{
    AutomaticUpgradeObservation, AutomaticUpgradePolicyProvider, DaemonRestart, DaemonUpgradeLease,
    DaemonUpgradePort, ProductBuildIdentity, ReleaseProcessPort, ReleaseTransport,
    SemanticAccelerator, SemanticLayoutPort, SemanticModelContract, SemanticModelVariant,
    UpgradeEngine, UpgradeFailureKind as EngineFailureKind, UpgradeObserver, UpgradeTerminalStatus,
};

use crate::{
    analytics::{
        count_bucket, OperationCompletedV1, Outcome, PublicEventV1, UpgradeChannel,
        UpgradeFailureKind, UpgradeMode, UpgradeOperation, UpgradeStatus, UpgradeTelemetry,
    },
    config::AppConfig,
};

pub(crate) static RELEASE_TRANSPORT: CliReleaseTransport = CliReleaseTransport;
pub(crate) static RELEASE_PROCESS: CliReleaseProcess = CliReleaseProcess;
pub(crate) static SEMANTIC_LAYOUT: CliSemanticLayout = CliSemanticLayout;
pub(crate) static DAEMON_UPGRADE: CliDaemonUpgrade = CliDaemonUpgrade;
pub(crate) static AUTOMATIC_POLICY: CliAutomaticUpgradePolicy = CliAutomaticUpgradePolicy;
pub(crate) static UPGRADE_OBSERVER: CliUpgradeObserver = CliUpgradeObserver;

pub(crate) const fn product_identity() -> ProductBuildIdentity {
    ProductBuildIdentity::new(env!("CARGO_PKG_VERSION"))
}

pub(crate) fn engine() -> UpgradeEngine<'static, CliDaemonUpgrade> {
    UpgradeEngine::new(
        product_identity(),
        &RELEASE_TRANSPORT,
        &RELEASE_PROCESS,
        &SEMANTIC_LAYOUT,
        &DAEMON_UPGRADE,
    )
}

pub(crate) struct CliReleaseTransport;

impl ReleaseTransport for CliReleaseTransport {
    fn get_bytes_limited(&self, endpoint: &str, max_bytes: usize) -> Result<Vec<u8>> {
        crate::net::get_bytes_limited(endpoint, max_bytes)
    }

    fn download_artifact(
        &self,
        endpoint: &str,
        destination: &mut File,
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<u64> {
        crate::net::download_artifact(endpoint, destination, max_bytes, timeout)
    }
}

pub(crate) struct CliReleaseProcess;

impl ReleaseProcessPort for CliReleaseProcess {
    fn sanitize_release_authority_env<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        crate::process_environment::sanitize_release_authority_env(command)
    }
}

pub(crate) struct CliSemanticLayout;

impl SemanticLayoutPort for CliSemanticLayout {
    fn native_accelerator(&self) -> Option<SemanticAccelerator> {
        crate::semantic::semantic_native_accelerator_target().map(|accelerator| match accelerator {
            crate::semantic::SemanticNativeAcceleratorTarget::CoreMl => SemanticAccelerator::CoreMl,
            crate::semantic::SemanticNativeAcceleratorTarget::WindowsMl => {
                SemanticAccelerator::WindowsMl
            }
            crate::semantic::SemanticNativeAcceleratorTarget::Cuda => SemanticAccelerator::OrtCuda,
        })
    }

    fn managed_model_snapshot_dir(&self, cache_root: &Path) -> PathBuf {
        crate::semantic::semantic_managed_model_snapshot_dir(cache_root)
    }

    fn worker_cache_dir(&self, data_root: &Path) -> PathBuf {
        crate::semantic::semantic_worker_cache_dir(data_root)
    }

    fn runtime_cache_dir(&self, data_root: &Path) -> PathBuf {
        crate::semantic::semantic_runtime_cache_dir(data_root)
    }

    fn model_contract_matches(&self, contract: &SemanticModelContract<'_>) -> bool {
        crate::semantic::semantic_provisioning_model_contract_matches(
            contract.model_id,
            contract.revision,
            contract.dimensions,
            contract.pooling,
            contract.normalization,
            contract.query_prefix,
            contract.passage_prefix,
        )
    }

    fn provisioning_model_path_count(&self) -> usize {
        crate::semantic::semantic_provisioning_model_path_count()
    }

    fn provisioning_model_path_matches(&self, path: &str) -> bool {
        crate::semantic::semantic_provisioning_model_path_matches(path)
    }

    fn required_model_file_count(&self, variant: SemanticModelVariant) -> usize {
        crate::semantic::semantic_required_model_file_count(semantic_variant(variant))
    }

    fn required_model_file_matches(
        &self,
        variant: SemanticModelVariant,
        path: &str,
        size: u64,
        sha256: &str,
    ) -> bool {
        crate::semantic::semantic_required_model_file_matches(
            semantic_variant(variant),
            path,
            size,
            sha256,
        )
    }

    fn provisioning_coreml_asset_matches(
        &self,
        artifact: &str,
        archive_sha256: &str,
        manifest_sha256: &str,
    ) -> bool {
        crate::semantic::semantic_provisioning_coreml_asset_matches(
            artifact,
            archive_sha256,
            manifest_sha256,
        )
    }
}

fn semantic_variant(variant: SemanticModelVariant) -> crate::semantic::SemanticOrtModelVariant {
    match variant {
        SemanticModelVariant::CpuFp32 => crate::semantic::SemanticOrtModelVariant::CpuFp32,
        SemanticModelVariant::AcceleratorO4Fp16 => {
            crate::semantic::SemanticOrtModelVariant::AcceleratorO4Fp16
        }
    }
}

pub(crate) struct CliDaemonUpgrade;

pub(crate) struct CliDaemonUpgradeLease(crate::semantic::DaemonUpgradeHandoff);

impl DaemonUpgradeLease for CliDaemonUpgradeLease {
    fn wait_for_installation_quiescence(&self) -> Result<()> {
        self.0.wait_for_installation_quiescence()
    }

    fn replacement_restart(&self) -> Option<DaemonRestart<'_>> {
        self.0
            .replacement_restart()
            .map(|(trigger, loop_interval_seconds)| DaemonRestart {
                trigger,
                loop_interval_seconds,
            })
    }

    fn resume_with(self, executable: &Path) -> Result<()> {
        self.0.resume_with(executable)
    }

    fn transfer_to_replacement_helper(self, helper_pid: u32) -> Result<()> {
        self.0.transfer_to_replacement_helper(helper_pid)
    }

    fn release_for_current_format_reexec(self) -> Result<()> {
        self.0.release_for_current_format_reexec()
    }
}

impl DaemonUpgradePort for CliDaemonUpgrade {
    type Lease = CliDaemonUpgradeLease;

    fn begin(&self, data_root: &Path, attempt_id: &str) -> Result<Self::Lease> {
        Ok(CliDaemonUpgradeLease(
            crate::semantic::begin_daemon_upgrade_handoff(data_root, attempt_id)?,
        ))
    }

    fn begin_legacy(
        &self,
        data_root: &Path,
        attempt_id: &str,
        target: &Path,
    ) -> Result<Self::Lease> {
        Ok(CliDaemonUpgradeLease(
            crate::semantic::begin_legacy_daemon_upgrade_handoff(data_root, attempt_id, target)?,
        ))
    }

    fn begin_current(
        &self,
        data_root: &Path,
        attempt_id: &str,
        restart_trigger: &str,
        loop_interval_seconds: Option<u64>,
    ) -> Result<Self::Lease> {
        let trigger = match restart_trigger {
            "setup" => crate::DaemonTriggerCommandArg::Setup,
            "import" => crate::DaemonTriggerCommandArg::Import,
            "search" => crate::DaemonTriggerCommandArg::Search,
            "semantic" => crate::DaemonTriggerCommandArg::Semantic,
            other => return Err(anyhow!("invalid daemon upgrade restart trigger {other}")),
        };
        Ok(CliDaemonUpgradeLease(
            crate::semantic::begin_current_daemon_upgrade_handoff(
                data_root,
                attempt_id,
                trigger,
                loop_interval_seconds,
            )?,
        ))
    }

    fn mark_replacement_helper_handoff(
        &self,
        data_root: &Path,
        attempt_id: &str,
        helper_pid: u32,
    ) -> Result<()> {
        crate::semantic::mark_replacement_helper_handoff(data_root, attempt_id, helper_pid)
    }

    fn replacement_helper_owns_handoff(
        &self,
        data_root: &Path,
        attempt_id: &str,
        helper_pid: u32,
    ) -> bool {
        crate::semantic::replacement_helper_owns_daemon_handoff(data_root, attempt_id, helper_pid)
    }

    fn complete_replacement_handoff(
        &self,
        data_root: &Path,
        executable: &Path,
        attempt_id: &str,
        restart: Option<DaemonRestart<'_>>,
    ) -> Result<()> {
        crate::semantic::complete_replacement_daemon_handoff(
            data_root,
            executable,
            attempt_id,
            restart.map(|restart| (restart.trigger, restart.loop_interval_seconds)),
        )
    }

    fn finish_replacement_handoff(&self, data_root: &Path, attempt_id: &str) -> Result<()> {
        crate::semantic::finish_replacement_daemon_handoff(data_root, attempt_id)
    }
}

pub(crate) struct CliAutomaticUpgradePolicy;

impl AutomaticUpgradePolicyProvider for CliAutomaticUpgradePolicy {
    type Snapshot = ctx_daemon_cli::DaemonConfigSnapshot;

    fn reload(&self, data_root: &Path) -> Result<Self::Snapshot> {
        AppConfig::load(data_root).map(|config| crate::semantic::daemon_config_snapshot(&config))
    }
}

pub(crate) struct CliUpgradeObserver;

impl UpgradeObserver<ctx_daemon_cli::DaemonConfigSnapshot> for CliUpgradeObserver {
    fn observe_automatic_terminal(
        &self,
        data_root: &Path,
        _config: &ctx_daemon_cli::DaemonConfigSnapshot,
        observation: AutomaticUpgradeObservation<'_>,
    ) {
        let plan = observation.plan;
        let failure_kind = observation.failure_kind.map(analytics_failure_kind);
        let event = PublicEventV1::OperationCompleted(OperationCompletedV1::for_automatic_upgrade(
            UpgradeTelemetry {
                mode: UpgradeMode::Auto,
                operation: UpgradeOperation::Apply,
                dry_run: false,
                suppress_event: false,
                status: Some(match observation.status {
                    UpgradeTerminalStatus::Applied => UpgradeStatus::Applied,
                    UpgradeTerminalStatus::Failed => UpgradeStatus::Failed,
                    UpgradeTerminalStatus::Skipped => UpgradeStatus::Skipped,
                    UpgradeTerminalStatus::UpToDate => UpgradeStatus::UpToDate,
                }),
                applied: Some(observation.applied),
                scheduled: Some(false),
                update_available: Some(false),
                update_was_available: plan.map(ctx_upgrade_engine::UpgradePlan::update_available),
                upgrade_attempt_id: Some(observation.attempt_id.to_owned()),
                managed_install: plan.map(ctx_upgrade_engine::UpgradePlan::managed),
                self_upgrade_allowed: plan
                    .map(ctx_upgrade_engine::UpgradePlan::self_upgrade_allowed),
                auto_upgrade_allowed: plan
                    .map(ctx_upgrade_engine::UpgradePlan::automatic_upgrade_allowed),
                warning_count: plan.map(|plan| count_bucket(plan.warnings().len() as u64)),
                channel: plan.map(|plan| UpgradeChannel::from_config(plan.channel())),
                failure_kind,
            },
            if failure_kind.is_some() {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            observation.duration,
        ));
        crate::semantic::deliver_daemon_events(data_root, &[event]);
    }
}

pub(super) fn analytics_failure_kind(kind: EngineFailureKind) -> UpgradeFailureKind {
    match kind {
        EngineFailureKind::LockFailed => UpgradeFailureKind::LockFailed,
        EngineFailureKind::UnmanagedInstall => UpgradeFailureKind::UnmanagedInstall,
        EngineFailureKind::MetadataFetch => UpgradeFailureKind::MetadataFetch,
        EngineFailureKind::SignatureVerify => UpgradeFailureKind::SignatureVerify,
        EngineFailureKind::MetadataInvalid => UpgradeFailureKind::MetadataInvalid,
        EngineFailureKind::ArtifactVerify => UpgradeFailureKind::ArtifactVerify,
        EngineFailureKind::ArtifactDownload => UpgradeFailureKind::ArtifactDownload,
        EngineFailureKind::PolicyDisallowed => UpgradeFailureKind::PolicyDisallowed,
        EngineFailureKind::ApplyFailed => UpgradeFailureKind::ApplyFailed,
    }
}
