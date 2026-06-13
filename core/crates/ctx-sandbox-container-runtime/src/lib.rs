use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::Digest;
use tokio::io::AsyncReadExt;

mod image;
mod sandbox_cli;

pub use ctx_harness_setup::{
    HarnessSetupDownloadStatus, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
    HarnessSetupProgressUpdate,
};
pub use image::{
    bundled_default_container_image_tar, container_image_present, container_image_status,
    default_container_image, default_container_image_fingerprint, ensure_container_image_available,
    ensure_managed_default_container_image_tar_with_source, force_reload_default_container_image,
    is_default_container_image, managed_default_image_install_lock, prefetch_container_image,
    prefetch_container_image_with_observer, prefetch_container_startup_artifacts_with_observer,
    ContainerImageStatus,
};
pub use sandbox_cli::{
    command_output_message, command_output_with_timeout, container_exists, container_running,
    ensure_workspace_volume, native_container_runtime_available, sandbox_cli_binary_path,
    sandbox_cli_env_for_data_root, sandbox_cli_env_for_mode, sandbox_cli_invocation,
    sandbox_container_command, sandbox_engine_ready, SandboxCliInvocation,
    SHARED_VM_SANDBOX_CLI_GUEST_BIN,
};

pub(crate) use ctx_harness_setup::{
    observe_log, observe_phase, observe_progress, ManagedArtifactDownloadReporter,
    ManagedDownloadAggregate,
};

pub const DEFAULT_CONTAINER_IMAGE: &str = "ghcr.io/ctxrs/ctx-harness:ubuntu-24.04";
pub const CTX_HARNESS_SANDBOX_CLI_PATH_ENV: &str = "CTX_HARNESS_SANDBOX_CLI_PATH";
const SANDBOX_INFO_TIMEOUT: Duration = Duration::from_secs(5);
const SANDBOX_OP_TIMEOUT: Duration = Duration::from_secs(60);
const SANDBOX_IMAGE_LOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxCommandMode {
    NativeContainer,
    SharedVm { helper_path: PathBuf },
}

pub fn resolve_container_image(configured_image: Option<&str>) -> String {
    if let Ok(value) = std::env::var("CTX_HARNESS_CONTAINER_IMAGE") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    configured_image
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CONTAINER_IMAGE)
        .to_string()
}

pub(crate) async fn sha256_hex_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buf)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn sandbox_cli_env_test_lock() -> &'static tokio::sync::Mutex<()> {
    use std::sync::OnceLock;

    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
