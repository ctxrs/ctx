mod archive;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use ctx_bundled_assets as bundled_assets;
use ctx_runtime_assets::download_managed_artifact;
use tokio::{fs, sync::Mutex};

use crate::{
    command_output_message, command_output_with_timeout, observe_log, observe_phase,
    observe_progress, sandbox_container_command, sandbox_engine_ready, sha256_hex_file,
    HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase, HarnessSetupProgressUpdate,
    ManagedArtifactDownloadReporter, ManagedDownloadAggregate, SandboxCommandMode,
    DEFAULT_CONTAINER_IMAGE, SANDBOX_IMAGE_LOAD_TIMEOUT, SANDBOX_OP_TIMEOUT,
};
use archive::load_container_image_tar;

fn image_load_heartbeat_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(5)
    }
}

fn image_load_poll_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(25)
    } else {
        Duration::from_secs(1)
    }
}

fn image_post_load_visibility_timeout() -> Duration {
    if cfg!(test) {
        Duration::from_millis(500)
    } else {
        Duration::from_secs(10)
    }
}

fn format_image_load_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    let minutes = secs / 60;
    let seconds = secs % 60;
    if minutes == 0 {
        format!("{seconds}s")
    } else {
        format!("{minutes}m {seconds}s")
    }
}

pub fn default_container_image() -> &'static str {
    DEFAULT_CONTAINER_IMAGE
}

pub fn is_default_container_image(image: &str) -> bool {
    image.trim() == DEFAULT_CONTAINER_IMAGE
}

pub fn bundled_default_container_image_tar() -> Option<PathBuf> {
    bundled_assets::bundled_ctx_harness_image_tar(DEFAULT_CONTAINER_IMAGE)
}

pub async fn default_container_image_fingerprint(image: &str) -> Result<Option<String>> {
    if image.trim() != DEFAULT_CONTAINER_IMAGE {
        return Ok(None);
    }
    if let Some(tar_path) = bundled_default_container_image_tar() {
        let digest = sha256_hex_file(&tar_path).await?;
        return Ok(Some(format!("sha256:{digest}")));
    }
    let Some(source) = bundled_assets::managed_ctx_harness_image_source(DEFAULT_CONTAINER_IMAGE)
    else {
        return Ok(None);
    };
    let digest = source.sha256.trim().to_ascii_lowercase();
    if digest.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("sha256:{digest}")))
}

pub async fn prefetch_container_startup_artifacts_with_observer(
    data_root: &Path,
    mode: &SandboxCommandMode,
    image: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    prefetch_container_startup_artifacts_with_source_override(
        data_root, mode, image, None, observer,
    )
    .await
}

async fn prefetch_container_startup_artifacts_with_source_override(
    data_root: &Path,
    _mode: &SandboxCommandMode,
    image: &str,
    source_override: Option<&bundled_assets::ManagedArtifactSource>,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("image is required");
    }
    let prefetch_default_image_tar =
        image == DEFAULT_CONTAINER_IMAGE && bundled_default_container_image_tar().is_none();
    let image_tar_download = async {
        if prefetch_default_image_tar {
            return ensure_managed_default_container_image_tar_with_override(
                data_root,
                source_override,
                observer,
                Some(ManagedDownloadAggregate::default()),
            )
            .await
            .map(Some);
        }
        Ok(None)
    };
    let prefetched_image_tar = image_tar_download.await?;
    if let Some(prefetched_image_tar) = prefetched_image_tar {
        observe_log(
            observer,
            HarnessSetupPhase::ImageCheck,
            HarnessSetupLogLevel::Info,
            &format!(
                "prefetched default harness image tar at {}",
                prefetched_image_tar.display()
            ),
        );
    }
    Ok(())
}

pub async fn prefetch_container_image_with_observer(
    data_root: &Path,
    mode: &SandboxCommandMode,
    image: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("image is required");
    }
    prefetch_container_startup_artifacts_with_source_override(
        data_root, mode, image, None, observer,
    )
    .await?;
    if !sandbox_engine_ready(data_root, mode).await.unwrap_or(false) {
        anyhow::bail!("native sandbox container runtime is not reachable for image prewarm");
    }
    observe_phase(
        observer,
        HarnessSetupPhase::ImageCheck,
        "checking harness image availability",
    );
    if container_image_present(data_root, mode, image).await? {
        observe_log(
            observer,
            HarnessSetupPhase::ImageCheck,
            HarnessSetupLogLevel::Info,
            "harness image already present",
        );
        return Ok(());
    }
    observe_phase(
        observer,
        HarnessSetupPhase::ImageLoad,
        "loading harness image into local sandbox runtime",
    );
    ensure_container_image_available(data_root, mode, image, observer).await
}

pub async fn prefetch_container_image(
    data_root: &Path,
    mode: &SandboxCommandMode,
    image: &str,
) -> Result<()> {
    prefetch_container_image_with_observer(data_root, mode, image, None).await
}

pub async fn container_image_present(
    data_root: &Path,
    mode: &SandboxCommandMode,
    image: &str,
) -> Result<bool> {
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("image is required");
    }
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("image").arg("inspect").arg(image);
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if output.status.success() {
        return Ok(true);
    }
    match output.status.code() {
        Some(1) => Ok(false),
        _ => anyhow::bail!(
            "container image inspect failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

pub async fn ensure_container_image_available(
    data_root: &Path,
    mode: &SandboxCommandMode,
    image: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("image is required");
    }

    if container_image_present(data_root, mode, image).await? {
        return Ok(());
    }

    if image == DEFAULT_CONTAINER_IMAGE {
        let image_tar = if let Some(tar) = bundled_assets::bundled_ctx_harness_image_tar(image) {
            observe_log(
                observer,
                HarnessSetupPhase::ImageLoad,
                HarnessSetupLogLevel::Info,
                &format!(
                    "loading default harness image from bundled tar {}",
                    tar.display()
                ),
            );
            tar
        } else {
            let managed_tar =
                ensure_managed_default_container_image_tar(data_root, observer).await?;
            observe_log(
                observer,
                HarnessSetupPhase::ImageLoad,
                HarnessSetupLogLevel::Info,
                &format!(
                    "loading default harness image from managed cache {}",
                    managed_tar.display()
                ),
            );
            managed_tar
        };
        load_container_image_tar(data_root, mode, &image_tar, image, observer).await?;
        return Ok(());
    }

    anyhow::bail!(
        "container image '{image}' is not present; registry pulls are disabled, so the image must already exist in the local sandbox runtime"
    );
}

pub async fn force_reload_default_container_image(
    data_root: &Path,
    mode: &SandboxCommandMode,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let image_tar = if let Some(tar) =
        bundled_assets::bundled_ctx_harness_image_tar(DEFAULT_CONTAINER_IMAGE)
    {
        observe_log(
            observer,
            HarnessSetupPhase::ImageLoad,
            HarnessSetupLogLevel::Info,
            &format!(
                "reloading default harness image from bundled tar {}",
                tar.display()
            ),
        );
        tar
    } else {
        let managed_tar = ensure_managed_default_container_image_tar(data_root, observer).await?;
        observe_log(
            observer,
            HarnessSetupPhase::ImageLoad,
            HarnessSetupLogLevel::Info,
            &format!(
                "reloading default harness image from managed cache {}",
                managed_tar.display()
            ),
        );
        managed_tar
    };
    load_container_image_tar(
        data_root,
        mode,
        &image_tar,
        DEFAULT_CONTAINER_IMAGE,
        observer,
    )
    .await
}

fn managed_default_container_image_tar_path(data_root: &Path, sha256: &str) -> PathBuf {
    data_root
        .join("managed")
        .join("images")
        .join("ctx-harness")
        .join("linux")
        .join(std::env::consts::ARCH)
        .join(format!("sha256-{}.tar", sha256.trim().to_ascii_lowercase()))
}

fn normalized_shared_vm_container_image_tar_path(data_root: &Path, sha256: &str) -> PathBuf {
    data_root
        .join("managed")
        .join("images")
        .join("docker-archive")
        .join(format!("sha256-{}.tar", sha256.trim().to_ascii_lowercase()))
}

pub fn managed_default_image_install_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn shared_vm_image_archive_normalization_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

async fn ensure_managed_default_container_image_tar(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<PathBuf> {
    ensure_managed_default_container_image_tar_with_override(data_root, None, observer, None).await
}

async fn ensure_managed_default_container_image_tar_with_override(
    data_root: &Path,
    source_override: Option<&bundled_assets::ManagedArtifactSource>,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<PathBuf> {
    let source = match source_override.cloned() {
        Some(source) => source,
        None => bundled_assets::managed_ctx_harness_image_source(DEFAULT_CONTAINER_IMAGE)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "default harness image is missing from bundle and runtime lock managed sources"
                )
            })?,
    };
    ensure_managed_default_container_image_tar_with_source(
        data_root,
        &source,
        observer,
        download_aggregate,
    )
    .await
}

pub async fn ensure_managed_default_container_image_tar_with_source(
    data_root: &Path,
    source: &bundled_assets::ManagedArtifactSource,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<PathBuf> {
    let _install_guard = managed_default_image_install_lock().lock().await;

    let final_tar = managed_default_container_image_tar_path(data_root, &source.sha256);
    if final_tar.exists() {
        let digest = sha256_hex_file(&final_tar)
            .await
            .with_context(|| format!("computing sha256 for {}", final_tar.display()))?;
        if digest.eq_ignore_ascii_case(source.sha256.trim()) {
            return Ok(final_tar);
        }
        observe_log(
            observer,
            HarnessSetupPhase::ImageLoad,
            HarnessSetupLogLevel::Warn,
            &format!(
                "managed image cache checksum mismatch for {}; re-downloading",
                final_tar.display()
            ),
        );
        let _ = fs::remove_file(&final_tar).await;
    }

    let Some(parent) = final_tar.parent() else {
        anyhow::bail!(
            "managed image cache path has no parent: {}",
            final_tar.display()
        );
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;
    let tmp_tar = final_tar.with_extension("download");

    observe_log(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        HarnessSetupLogLevel::Info,
        &format!("downloading default harness image from {}", source.uri),
    );
    download_managed_artifact(
        &source.uri,
        &tmp_tar,
        Some(ManagedArtifactDownloadReporter::new(
            observer,
            download_aggregate,
            HarnessSetupPhase::ArtifactDownload,
            "Harness image",
        )),
    )
    .await?;

    let digest = sha256_hex_file(&tmp_tar)
        .await
        .with_context(|| format!("computing sha256 for {}", tmp_tar.display()))?;
    if !digest.eq_ignore_ascii_case(source.sha256.trim()) {
        let _ = fs::remove_file(&tmp_tar).await;
        anyhow::bail!(
            "managed harness image checksum mismatch: expected {}, got {}",
            source.sha256.trim(),
            digest
        );
    }
    fs::rename(&tmp_tar, &final_tar).await.with_context(|| {
        format!(
            "moving managed image tar into place: {} -> {}",
            tmp_tar.display(),
            final_tar.display()
        )
    })?;
    Ok(final_tar)
}

#[derive(Debug, Clone)]
pub struct ContainerImageStatus {
    pub present: bool,
    pub available: bool,
    pub error: Option<String>,
}

pub async fn container_image_status(
    data_root: &Path,
    mode: &SandboxCommandMode,
    image: &str,
) -> Result<ContainerImageStatus> {
    let image = image.trim();
    if image.is_empty() {
        anyhow::bail!("image is required");
    }
    let output = match sandbox_container_command(data_root, mode) {
        Ok(mut cmd) => {
            cmd.arg("image").arg("inspect").arg(image);
            command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await
        }
        Err(err) => {
            return Ok(ContainerImageStatus {
                present: false,
                available: false,
                error: Some(err.to_string()),
            });
        }
    };
    let output = match output {
        Ok(out) => out,
        Err(err) => {
            return Ok(ContainerImageStatus {
                present: false,
                available: false,
                error: Some(err.to_string()),
            });
        }
    };
    if output.status.success() {
        return Ok(ContainerImageStatus {
            present: true,
            available: true,
            error: None,
        });
    }
    match output.status.code() {
        Some(1) => Ok(ContainerImageStatus {
            present: false,
            available: true,
            error: None,
        }),
        _ => Ok(ContainerImageStatus {
            present: false,
            available: false,
            error: Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        }),
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
