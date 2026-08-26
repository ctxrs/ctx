use std::{
    env,
    fs::File,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

mod command;
mod diagnostics;
mod download;
mod install;
mod legacy_automatic;
mod metadata;
mod state;
mod version;
mod version_probe;

pub use command::{PreparedAutomaticUpgrade, UpgradeOutcome};
pub use diagnostics::{
    managed_install_executable, upgrade_diagnostics, ManagedInstallDiagnostic, UpgradeDiagnostics,
};
pub use install::{
    current_exe_has_managed_install_marker_hint, current_exe_is_unmanaged, current_install_path,
    installation_hosted_uninstall_is_active,
    installation_hosted_uninstall_is_active_for_executable,
    invalid_install_marker_recovery_guidance, is_valid_install_attempt_id,
    managed_install_marker_for_current_exe, run_hosted_transaction,
    unmanaged_install_conversion_guidance, HostedTransactionAction, HostedTransactionArgs,
    InstallMarker, ManagedInstallMarker,
};
pub use state::{
    active_installation_upgrade_attempt_id, automatic_upgrade_check_due,
    installation_daemon_coordination_paths, installation_daemon_coordination_paths_for,
    installation_executable_path, installation_upgrade_is_active, is_valid_upgrade_attempt_id,
    read_state_json, terminal_installation_upgrade_attempt_id, STATE_SCHEMA_VERSION,
};

/// Product identity supplied by the ctx composition root.
///
/// This value deliberately does not derive from this package's Cargo version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductBuildIdentity {
    version: &'static str,
}

/// Bounded release-network operations supplied by the CLI.
pub trait ReleaseTransport: Send + Sync {
    fn get_bytes_limited(&self, endpoint: &str, max_bytes: usize) -> Result<Vec<u8>>;

    fn download_artifact(
        &self,
        endpoint: &str,
        destination: &mut File,
        max_bytes: u64,
        timeout: Duration,
    ) -> Result<u64>;
}

/// Child-process release-authority controls supplied by the CLI.
pub trait ReleaseProcessPort: Send + Sync {
    fn sanitize_release_authority_env<'a>(&self, command: &'a mut Command) -> &'a mut Command;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticAccelerator {
    CoreMl,
    WindowsMl,
    OrtCuda,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticModelVariant {
    CpuFp32,
    AcceleratorO4Fp16,
}

impl SemanticModelVariant {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CpuFp32 => "cpu-fp32",
            Self::AcceleratorO4Fp16 => "accelerator-o4-fp16",
        }
    }
}

pub struct SemanticModelContract<'a> {
    pub model_id: &'a str,
    pub revision: &'a str,
    pub dimensions: u32,
    pub pooling: &'a str,
    pub normalization: &'a str,
    pub query_prefix: &'a str,
    pub passage_prefix: &'a str,
}

/// Semantic layout and compiled-model facts supplied by ctx composition.
pub trait SemanticLayoutPort: Send + Sync {
    fn native_accelerator(&self) -> Option<SemanticAccelerator>;
    fn managed_model_snapshot_dir(&self, cache_root: &Path) -> PathBuf;
    fn worker_cache_dir(&self, data_root: &Path) -> PathBuf;
    fn runtime_cache_dir(&self, data_root: &Path) -> PathBuf;
    fn model_contract_matches(&self, contract: &SemanticModelContract<'_>) -> bool;
    fn provisioning_model_path_count(&self) -> usize;
    fn provisioning_model_path_matches(&self, path: &str) -> bool;
    fn required_model_file_count(&self, variant: SemanticModelVariant) -> usize;
    fn required_model_file_matches(
        &self,
        variant: SemanticModelVariant,
        path: &str,
        size: u64,
        sha256: &str,
    ) -> bool;
    fn provisioning_coreml_asset_matches(
        &self,
        artifact: &str,
        archive_sha256: &str,
        manifest_sha256: &str,
    ) -> bool;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DaemonRestart<'a> {
    pub trigger: &'a str,
    pub loop_interval_seconds: Option<u64>,
}

pub trait DaemonUpgradeLease: Send + Sized {
    fn wait_for_installation_quiescence(&self) -> Result<()>;
    fn replacement_restart(&self) -> Option<DaemonRestart<'_>>;
    fn resume_with(self, executable: &Path) -> Result<()>;
    fn transfer_to_replacement_helper(self, helper_pid: u32) -> Result<()>;
    fn release_for_current_format_reexec(self) -> Result<()>;
}

pub trait DaemonUpgradePort: Send + Sync {
    type Lease: DaemonUpgradeLease;

    fn begin(&self, data_root: &Path, attempt_id: &str) -> Result<Self::Lease>;

    fn begin_legacy(
        &self,
        data_root: &Path,
        attempt_id: &str,
        target: &Path,
    ) -> Result<Self::Lease>;

    fn begin_current(
        &self,
        data_root: &Path,
        attempt_id: &str,
        restart_trigger: &str,
        loop_interval_seconds: Option<u64>,
    ) -> Result<Self::Lease>;

    fn mark_replacement_helper_handoff(
        &self,
        data_root: &Path,
        attempt_id: &str,
        helper_pid: u32,
    ) -> Result<()>;

    fn replacement_helper_owns_handoff(
        &self,
        data_root: &Path,
        attempt_id: &str,
        helper_pid: u32,
    ) -> bool;

    fn complete_replacement_handoff(
        &self,
        data_root: &Path,
        executable: &Path,
        attempt_id: &str,
        restart: Option<DaemonRestart<'_>>,
    ) -> Result<()>;

    fn finish_replacement_handoff(&self, data_root: &Path, attempt_id: &str) -> Result<()>;
}

pub trait AutomaticUpgradePolicySnapshot {
    fn automatic_upgrade_enabled(&self) -> bool;
    fn interval(&self) -> Duration;
    fn channel(&self) -> &str;
    fn semantic_enabled(&self) -> bool;
}

pub trait AutomaticUpgradePolicyProvider {
    type Snapshot: AutomaticUpgradePolicySnapshot;

    fn reload(&self, data_root: &Path) -> Result<Self::Snapshot>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradeTerminalStatus {
    Applied,
    Failed,
    Skipped,
    UpToDate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradeFailureKind {
    LockFailed,
    UnmanagedInstall,
    MetadataFetch,
    SignatureVerify,
    MetadataInvalid,
    ArtifactVerify,
    ArtifactDownload,
    PolicyDisallowed,
    ApplyFailed,
}

pub struct AutomaticUpgradeObservation<'a> {
    pub plan: Option<&'a UpgradePlan>,
    pub attempt_id: &'a str,
    pub status: UpgradeTerminalStatus,
    pub applied: bool,
    pub failure_kind: Option<UpgradeFailureKind>,
    pub duration: Duration,
}

pub trait UpgradeObserver<S: AutomaticUpgradePolicySnapshot> {
    fn observe_automatic_terminal(
        &self,
        data_root: &Path,
        policy: &S,
        observation: AutomaticUpgradeObservation<'_>,
    );
}

#[derive(Clone, Copy, Debug)]
pub struct UpgradePolicy<'a> {
    pub channel: &'a str,
    pub interval: Duration,
    pub semantic_enabled: bool,
}

pub struct UpgradeEngine<'a, D: DaemonUpgradePort + ?Sized> {
    identity: ProductBuildIdentity,
    transport: &'a dyn ReleaseTransport,
    process: &'a dyn ReleaseProcessPort,
    semantic_layout: &'a dyn SemanticLayoutPort,
    daemon: &'a D,
}

impl<'a, D: DaemonUpgradePort + ?Sized> UpgradeEngine<'a, D> {
    pub const fn new(
        identity: ProductBuildIdentity,
        transport: &'a dyn ReleaseTransport,
        process: &'a dyn ReleaseProcessPort,
        semantic_layout: &'a dyn SemanticLayoutPort,
        daemon: &'a D,
    ) -> Self {
        Self {
            identity,
            transport,
            process,
            semantic_layout,
            daemon,
        }
    }

    pub fn run_legacy_automatic_bridge(&self) -> Result<bool> {
        legacy_automatic::run_legacy_automatic_upgrade_bridge(self.daemon)
    }
}

impl ProductBuildIdentity {
    pub const fn new(version: &'static str) -> Self {
        Self { version }
    }

    pub const fn version(self) -> &'static str {
        self.version
    }
}

#[derive(Debug, Clone)]
pub struct UpgradePlan {
    current_version: String,
    latest_version: String,
    channel: String,
    platform: String,
    metadata_url: String,
    artifact_url: String,
    artifact_sha256: String,
    install_path: std::path::PathBuf,
    #[cfg_attr(windows, allow(dead_code))]
    install_fingerprint: install::InstallFingerprint,
    update_available: bool,
    managed: bool,
    warnings: Vec<String>,
    metadata: metadata::ReleaseMetadata,
    semantic_provisioning: Option<metadata::SelectedSemanticProvisioning>,
}

impl UpgradePlan {
    fn onnxruntime_artifact_url(&self) -> Option<String> {
        self.metadata.onnxruntime.as_ref().map(|runtime| {
            format!(
                "{}/{}",
                self.metadata.base_url.trim_end_matches('/'),
                runtime.artifact
            )
        })
    }

    fn semantic_artifact_url(&self, artifact: &str) -> String {
        format!(
            "{}/{}",
            self.metadata.base_url.trim_end_matches('/'),
            artifact
        )
    }

    pub fn current_version(&self) -> &str {
        &self.current_version
    }

    pub fn latest_version(&self) -> &str {
        &self.latest_version
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn metadata_url(&self) -> &str {
        &self.metadata_url
    }

    pub fn artifact_url(&self) -> &str {
        &self.artifact_url
    }

    pub fn install_path(&self) -> &Path {
        &self.install_path
    }

    pub fn update_available(&self) -> bool {
        self.update_available
    }

    pub fn managed(&self) -> bool {
        self.managed
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn self_upgrade_allowed(&self) -> bool {
        self.metadata.self_upgrade_allowed
    }

    pub fn automatic_upgrade_allowed(&self) -> bool {
        self.metadata.auto_upgrade_allowed
    }
}

fn platform_key() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x64"),
        ("windows", "x86_64") => Ok("windows-x64"),
        (os, arch) => Err(anyhow!("unsupported ctx upgrade platform: {os}-{arch}")),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn env_flag(key: &str) -> bool {
    env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}

const fn test_harness_enabled() -> bool {
    cfg!(debug_assertions)
        || cfg!(ctx_upgrade_engine_test_support)
        || cfg!(feature = "test-support")
        || option_env!("CTX_UPGRADE_TEST_HARNESS").is_some()
}

fn version_gt(left: &str, right: &str) -> bool {
    version::version_gt(left, right)
}

#[cfg(test)]
struct TestReleaseProcess;

#[cfg(test)]
impl ReleaseProcessPort for TestReleaseProcess {
    fn sanitize_release_authority_env<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command
    }
}

#[cfg(test)]
static TEST_RELEASE_PROCESS: TestReleaseProcess = TestReleaseProcess;

#[cfg(test)]
struct TestSemanticLayout;

#[cfg(test)]
impl SemanticLayoutPort for TestSemanticLayout {
    fn native_accelerator(&self) -> Option<SemanticAccelerator> {
        None
    }

    fn managed_model_snapshot_dir(&self, cache_root: &Path) -> PathBuf {
        cache_root
            .join("ctx-semantic-models")
            .join("models--intfloat--multilingual-e5-small")
            .join("snapshots")
            .join("614241f622f53c4eeff9890bdc4f31cfecc418b3")
    }

    fn worker_cache_dir(&self, data_root: &Path) -> PathBuf {
        data_root.join("semantic-model-cache")
    }

    fn runtime_cache_dir(&self, data_root: &Path) -> PathBuf {
        data_root.join("runtime")
    }

    fn model_contract_matches(&self, contract: &SemanticModelContract<'_>) -> bool {
        contract.model_id == "intfloat/multilingual-e5-small"
            && contract.revision == "614241f622f53c4eeff9890bdc4f31cfecc418b3"
            && contract.dimensions == 384
            && contract.pooling == "attention_mask_mean"
            && contract.normalization == "l2"
            && contract.query_prefix == "query: "
            && contract.passage_prefix == "passage: "
    }

    fn provisioning_model_path_count(&self) -> usize {
        7
    }

    fn provisioning_model_path_matches(&self, path: &str) -> bool {
        matches!(
            path,
            "LICENSE"
                | "config.json"
                | "manifest.json"
                | "onnx/model.onnx"
                | "special_tokens_map.json"
                | "tokenizer.json"
                | "tokenizer_config.json"
        )
    }

    fn required_model_file_count(&self, _variant: SemanticModelVariant) -> usize {
        7
    }

    fn required_model_file_matches(
        &self,
        variant: SemanticModelVariant,
        path: &str,
        size: u64,
        sha256: &str,
    ) -> bool {
        if path != "onnx/model.onnx" {
            return self.provisioning_model_path_matches(path);
        }
        match variant {
            SemanticModelVariant::CpuFp32 => {
                size == 470_268_510
                    && sha256 == "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665"
            }
            SemanticModelVariant::AcceleratorO4Fp16 => {
                size == 235_052_531
                    && sha256 == "4654c156f3e4171abc9c716cdb771bf9116455d15ac1aab364aeeede0e3205b0"
            }
        }
    }

    fn provisioning_coreml_asset_matches(
        &self,
        artifact: &str,
        archive_sha256: &str,
        manifest_sha256: &str,
    ) -> bool {
        artifact == "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz"
            && archive_sha256 == "25fbf333d1e72f5c075973ef968dfa1446459f61f3ac63ef3690d9865435af17"
            && manifest_sha256 == "20a94162aca7c2f9f65be27839cd6867ec1c54e142fdf0c652de20139dffbc19"
    }
}

#[cfg(test)]
static TEST_SEMANTIC_LAYOUT: TestSemanticLayout = TestSemanticLayout;
