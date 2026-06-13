use super::*;

fn managed_sandbox_machine_cache_install_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(in crate::daemon::workspace_runtime) async fn ensure_managed_sandbox_machine_cache(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<PathBuf> {
    let source = bundled_assets::managed_sandbox_machine_cache_source().ok_or_else(|| {
        anyhow::anyhow!(
            "managed sandbox machine cache source is not available for {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let final_path = managed_sandbox_machine_cache_path(data_root, &source);
    let partial_path = managed_artifact_partial_path(&final_path);
    let _install_guard = managed_sandbox_machine_cache_install_lock().lock().await;
    let _cross_process_guard = acquire_managed_artifact_file_lock(
        &managed_artifact_lock_path(&final_path),
        "managed sandbox machine cache",
        observer,
        HarnessSetupPhase::ArtifactDownload,
    )
    .await?;
    if final_path.exists() {
        let digest = updates::sha256_hex_file(&final_path)
            .await
            .with_context(|| format!("computing sha256 for {}", final_path.display()))?;
        if digest.eq_ignore_ascii_case(source.sha256.trim()) {
            let _ = fs::remove_file(&partial_path).await;
            return Ok(final_path);
        }
        observe_log(
            observer,
            HarnessSetupPhase::ArtifactDownload,
            HarnessSetupLogLevel::Warn,
            &format!(
                "managed sandbox machine cache checksum mismatch for {}; re-downloading",
                final_path.display()
            ),
        );
        let _ = fs::remove_file(&final_path).await;
    }

    let Some(parent) = final_path.parent() else {
        anyhow::bail!(
            "managed sandbox machine cache path has no parent: {}",
            final_path.display()
        );
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;
    observe_log(
        observer,
        HarnessSetupPhase::ArtifactDownload,
        HarnessSetupLogLevel::Info,
        &format!(
            "downloading managed sandbox machine cache into {}",
            final_path.display()
        ),
    );
    download_managed_artifact(
        &source.uri,
        &partial_path,
        Some(ManagedArtifactDownloadReporter::new(
            observer,
            download_aggregate,
            HarnessSetupPhase::ArtifactDownload,
            "Sandbox machine cache",
        )),
    )
    .await?;
    finalize_managed_artifact_download(
        &partial_path,
        &final_path,
        &source.sha256,
        "managed sandbox machine cache",
    )
    .await?;
    Ok(final_path)
}
