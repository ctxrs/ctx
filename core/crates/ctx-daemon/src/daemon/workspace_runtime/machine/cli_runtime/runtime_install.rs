use super::*;

mod archive;
mod helpers;
mod paths;
#[cfg(test)]
mod tests;

use archive::{acquire_managed_sandbox_cli_archive, install_managed_sandbox_cli_archive};
use helpers::{download_managed_sandbox_cli_helpers, mark_runtime_artifacts_executable};
use paths::{
    managed_sandbox_cli_install_lock, managed_sandbox_cli_runtime_bin_path,
    managed_sandbox_cli_runtime_is_ready, managed_sandbox_cli_runtime_root,
    managed_sandbox_cli_runtime_source, mark_managed_sandbox_cli_runtime_ready,
};

pub(in crate::daemon::workspace_runtime) async fn ensure_managed_sandbox_cli_runtime(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<PathBuf> {
    ensure_managed_sandbox_cli_runtime_with_override(data_root, None, observer, download_aggregate)
        .await
}

async fn ensure_managed_sandbox_cli_runtime_with_override(
    data_root: &Path,
    source_override: Option<&bundled_assets::ManagedRuntimeSource>,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<PathBuf> {
    if source_override.is_none() {
        if let Ok(raw) = std::env::var(CTX_HARNESS_SANDBOX_CLI_PATH_ENV) {
            let path = PathBuf::from(raw.trim());
            if path.exists() {
                return Ok(path);
            }
        }
        if let Some(bundled) = bundled_assets::bundled_sandbox_cli_runtime() {
            return Ok(bundled.bin);
        }
    }
    let source = match source_override.cloned() {
        Some(source) => source,
        None => managed_sandbox_cli_runtime_source().ok_or_else(|| {
            anyhow::anyhow!(
                "managed sandbox CLI runtime source is not available for {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?,
    };
    let runtime_root = managed_sandbox_cli_runtime_root(data_root, &source);
    let runtime_bin = managed_sandbox_cli_runtime_bin_path(data_root, &source);
    if managed_sandbox_cli_runtime_is_ready(&runtime_root, &runtime_bin, &source) {
        return Ok(runtime_bin);
    }
    let _install_guard = managed_sandbox_cli_install_lock().lock().await;
    if managed_sandbox_cli_runtime_is_ready(&runtime_root, &runtime_bin, &source) {
        return Ok(runtime_bin);
    }

    observe_log(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        HarnessSetupLogLevel::Info,
        &format!("installing managed sandbox CLI runtime {}", source.version),
    );

    let final_archive = acquire_managed_sandbox_cli_archive(
        data_root,
        &source,
        observer,
        download_aggregate.clone(),
    )
    .await?;
    install_managed_sandbox_cli_archive(&final_archive, &source, &runtime_root).await?;
    download_managed_sandbox_cli_helpers(
        &source,
        &runtime_root,
        observer,
        download_aggregate.clone(),
    )
    .await?;

    if !runtime_bin.exists() {
        anyhow::bail!(
            "managed sandbox CLI runtime installed but binary is missing at {}",
            runtime_bin.display()
        );
    }
    mark_runtime_artifacts_executable(&runtime_root, &runtime_bin, &source).await?;
    mark_managed_sandbox_cli_runtime_ready(&runtime_root).await?;
    Ok(runtime_bin)
}
