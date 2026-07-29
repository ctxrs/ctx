mod command;
mod diagnostics;
mod download;
mod install;
mod metadata;
mod path;
mod state;
mod version;

pub(crate) use command::{
    finish_daemon_auto_upgrade, prepare_daemon_auto_upgrade, PreparedDaemonUpgrade,
};
pub use command::{run, UpgradeArgs};
pub(crate) use diagnostics::managed_install_executable;
pub(crate) use diagnostics::upgrade_diagnostics;
pub(crate) use install::is_valid_install_attempt_id;
pub(crate) use state::{
    active_installation_upgrade_attempt_id, installation_daemon_coordination_paths,
    installation_daemon_coordination_paths_for, installation_executable_path,
    installation_upgrade_is_active, is_valid_upgrade_attempt_id,
    terminal_installation_upgrade_attempt_id,
};

use std::env;

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
struct UpgradePlan {
    pub(super) current_version: String,
    pub(super) latest_version: String,
    pub(super) channel: String,
    pub(super) platform: String,
    pub(super) metadata_url: String,
    pub(super) artifact_url: String,
    pub(super) artifact_sha256: String,
    pub(super) install_path: std::path::PathBuf,
    #[cfg_attr(windows, allow(dead_code))]
    pub(super) install_fingerprint: install::InstallFingerprint,
    pub(super) update_available: bool,
    pub(super) managed: bool,
    pub(super) warnings: Vec<String>,
    pub(super) path: path::PathDiagnostics,
    pub(super) metadata: metadata::ReleaseMetadata,
    pub(super) semantic_provisioning: Option<metadata::SelectedSemanticProvisioning>,
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
}

fn platform_key() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-x64"),
        ("windows", "x86_64") => Ok("windows-x64"),
        ("freebsd", "x86_64") => Ok("freebsd-x64"),
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

pub(in crate::upgrade) const fn test_harness_enabled() -> bool {
    cfg!(debug_assertions) || option_env!("CTX_UPGRADE_TEST_HARNESS").is_some()
}

fn version_gt(left: &str, right: &str) -> bool {
    version::version_gt(left, right)
}
