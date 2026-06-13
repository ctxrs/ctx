use super::*;

pub(super) async fn acquire_managed_sandbox_cli_archive(
    data_root: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<PathBuf> {
    let final_archive = managed_sandbox_cli_archive_path(data_root, source);
    let partial_archive = managed_artifact_partial_path(&final_archive);
    if final_archive.exists() {
        let digest = updates::sha256_hex_file(&final_archive)
            .await
            .with_context(|| format!("computing sha256 for {}", final_archive.display()))?;
        if !digest.eq_ignore_ascii_case(source.sha256.trim()) {
            let _ = fs::remove_file(&final_archive).await;
        } else {
            let _ = fs::remove_file(&partial_archive).await;
        }
    }
    if !final_archive.exists() {
        let Some(parent) = final_archive.parent() else {
            anyhow::bail!(
                "managed sandbox CLI archive path has no parent: {}",
                final_archive.display()
            );
        };
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
        download_managed_artifact(
            &source.uri,
            &partial_archive,
            Some(ManagedArtifactDownloadReporter::new(
                observer,
                download_aggregate,
                HarnessSetupPhase::ArtifactDownload,
                "Sandbox CLI runtime",
            )),
        )
        .await?;
        let digest = updates::sha256_hex_file(&partial_archive)
            .await
            .with_context(|| format!("computing sha256 for {}", partial_archive.display()))?;
        if !digest.eq_ignore_ascii_case(source.sha256.trim()) {
            let _ = fs::remove_file(&partial_archive).await;
            anyhow::bail!(
                "managed sandbox CLI runtime checksum mismatch: expected {}, got {}",
                source.sha256.trim(),
                digest
            );
        }
        finalize_managed_artifact_download(
            &partial_archive,
            &final_archive,
            &source.sha256,
            "managed sandbox CLI runtime archive",
        )
        .await?;
    }
    Ok(final_archive)
}

pub(super) async fn install_managed_sandbox_cli_archive(
    final_archive: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
    runtime_root: &Path,
) -> Result<()> {
    let Some(parent) = runtime_root.parent() else {
        anyhow::bail!(
            "managed runtime root has no parent: {}",
            runtime_root.display()
        );
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;
    let staging_dir = parent.join(format!(
        ".sandbox-cli-staging-{}",
        uuid::Uuid::new_v4().simple()
    ));
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir).await;
    }
    fs::create_dir_all(&staging_dir)
        .await
        .with_context(|| format!("creating {}", staging_dir.display()))?;
    let extract_dir = staging_dir.join("extract");
    fs::create_dir_all(&extract_dir)
        .await
        .with_context(|| format!("creating {}", extract_dir.display()))?;
    let archive_for_extract = final_archive.to_path_buf();
    let uri_for_extract = source.uri.clone();
    let extract_dir_for_extract = extract_dir.clone();
    tokio::task::spawn_blocking(move || {
        extract_archive_to_dir(
            &archive_for_extract,
            &uri_for_extract,
            &extract_dir_for_extract,
        )
    })
    .await
    .context("joining managed sandbox CLI extract task")??;
    let extracted_root = tokio::task::spawn_blocking({
        let extract_dir = extract_dir.clone();
        move || resolve_single_extracted_root(&extract_dir)
    })
    .await
    .context("joining managed sandbox CLI extraction root task")??;

    if runtime_root.exists() {
        let _ = fs::remove_dir_all(runtime_root).await;
    }
    fs::rename(&extracted_root, runtime_root)
        .await
        .with_context(|| {
            format!(
                "moving extracted sandbox CLI runtime into place: {} -> {}",
                extracted_root.display(),
                runtime_root.display()
            )
        })?;
    let _ = fs::remove_dir_all(&staging_dir).await;
    Ok(())
}
