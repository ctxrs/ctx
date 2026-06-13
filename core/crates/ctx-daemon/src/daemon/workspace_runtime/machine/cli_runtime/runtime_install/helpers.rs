use super::paths::managed_sandbox_cli_helper_path;
use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(super) async fn download_managed_sandbox_cli_helpers(
    source: &bundled_assets::ManagedRuntimeSource,
    runtime_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<()> {
    let mut helper_downloads = Vec::new();
    for (name, helper) in &source.helpers {
        let Some(path) = managed_sandbox_cli_helper_path(runtime_root, name) else {
            continue;
        };
        let helper_name = name.to_string();
        let helper_source = helper.clone();
        let helper_path = path.clone();
        let aggregate = download_aggregate.clone();
        helper_downloads.push(async move {
            if let Some(parent) = helper_path.parent() {
                fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            if helper_path.exists() {
                let digest = updates::sha256_hex_file(&helper_path)
                    .await
                    .with_context(|| format!("computing sha256 for {}", helper_path.display()))?;
                if digest.eq_ignore_ascii_case(helper_source.sha256.trim()) {
                    let _ = fs::remove_file(managed_artifact_partial_path(&helper_path)).await;
                    return Ok(()) as Result<()>;
                }
                let _ = fs::remove_file(&helper_path).await;
            }
            let tmp = managed_artifact_partial_path(&helper_path);
            download_managed_artifact(
                &helper_source.uri,
                &tmp,
                Some(ManagedArtifactDownloadReporter::new(
                    observer,
                    aggregate,
                    HarnessSetupPhase::ArtifactDownload,
                    format!("Sandbox helper ({helper_name})"),
                )),
            )
            .await?;
            let digest = updates::sha256_hex_file(&tmp)
                .await
                .with_context(|| format!("computing sha256 for {}", tmp.display()))?;
            if !digest.eq_ignore_ascii_case(helper_source.sha256.trim()) {
                let _ = fs::remove_file(&tmp).await;
                anyhow::bail!(
                    "managed sandbox helper checksum mismatch ({}): expected {}, got {}",
                    helper_name,
                    helper_source.sha256.trim(),
                    digest
                );
            }
            fs::rename(&tmp, &helper_path).await.with_context(|| {
                format!(
                    "moving managed sandbox helper into place: {} -> {}",
                    tmp.display(),
                    helper_path.display()
                )
            })?;
            Ok(())
        });
    }
    for result in futures::future::join_all(helper_downloads).await {
        result?;
    }
    Ok(())
}

pub(super) async fn mark_runtime_artifacts_executable(
    runtime_root: &Path,
    runtime_bin: &Path,
    source: &bundled_assets::ManagedRuntimeSource,
) -> Result<()> {
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(runtime_bin)
            .await
            .with_context(|| format!("metadata {}", runtime_bin.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(runtime_bin, perms)
            .await
            .with_context(|| format!("chmod {}", runtime_bin.display()))?;
        for name in source.helpers.keys() {
            if let Some(helper_path) = managed_sandbox_cli_helper_path(runtime_root, name) {
                if helper_path.exists() {
                    let mut helper_perms = fs::metadata(&helper_path)
                        .await
                        .with_context(|| format!("metadata {}", helper_path.display()))?
                        .permissions();
                    helper_perms.set_mode(0o755);
                    fs::set_permissions(&helper_path, helper_perms)
                        .await
                        .with_context(|| format!("chmod {}", helper_path.display()))?;
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (runtime_root, runtime_bin, source);
    }
    Ok(())
}
