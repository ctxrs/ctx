use super::*;
use ctx_bundled_assets as bundled_assets;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex as StdMutex;
#[cfg(any(test, feature = "test-support"))]
use std::sync::MutexGuard as StdMutexGuard;
use std::sync::OnceLock;
use std::time::Instant;

#[path = "runtime_install/assets.rs"]
mod assets;
#[path = "runtime_install/staged.rs"]
mod staged;
#[cfg(test)]
mod tests;

use assets::*;
pub(super) use assets::{avf_linux_runtime_is_ready, managed_avf_linux_guest_source};
#[cfg(test)]
pub(super) use assets::{
    managed_avf_linux_archive_path, managed_avf_linux_runtime_ready_marker_path,
    managed_avf_linux_runtime_source_identity,
};
use staged::*;
pub(super) use staged::{bundled_avf_linux_guest_runtime, staged_avf_linux_guest_runtime};

pub fn runtime_target_label() -> String {
    if explicit_staged_avf_linux_guest_runtime_dir().is_some() {
        return format!("{AVF_LINUX_GUEST_RUNTIME_ID}:staged");
    }
    if let Some(runtime) = bundled_avf_linux_guest_runtime() {
        return format!("{AVF_LINUX_GUEST_RUNTIME_ID}:bundled:{}", runtime.version);
    }
    managed_avf_linux_guest_source()
        .map(|source| format!("{AVF_LINUX_GUEST_RUNTIME_ID}:{}", source.version.trim()))
        .unwrap_or_else(|| AVF_LINUX_GUEST_RUNTIME_ID.to_string())
}

pub(super) fn runtime_ready(data_root: &Path) -> Result<bool> {
    if let Some(runtime) = staged_avf_linux_guest_runtime()? {
        return Ok(avf_linux_runtime_is_ready(&runtime));
    }
    if let Some(runtime) = bundled_avf_linux_guest_runtime() {
        return Ok(avf_linux_runtime_is_ready(&runtime));
    }
    if let Some(source) = managed_avf_linux_guest_source() {
        let runtime = AvfLinuxGuestRuntime::from_source(data_root, &source)?;
        return Ok(avf_linux_runtime_is_ready(&runtime));
    }
    Ok(false)
}

pub async fn ensure_managed_avf_linux_guest_runtime(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<AvfLinuxGuestRuntime> {
    ensure_managed_avf_linux_guest_runtime_with_override(
        data_root,
        None,
        observer,
        download_aggregate,
    )
    .await
}

pub async fn ensure_managed_avf_linux_guest_runtime_with_override(
    data_root: &Path,
    source_override: Option<&bundled_assets::ManagedRuntimeSource>,
    observer: Option<&dyn HarnessSetupObserver>,
    download_aggregate: Option<ManagedDownloadAggregate>,
) -> Result<AvfLinuxGuestRuntime> {
    let install_started = Instant::now();
    if source_override.is_none() {
        if let Some(runtime) = staged_avf_linux_guest_runtime()? {
            return Ok(runtime);
        }
        if let Some(runtime) = bundled_avf_linux_guest_runtime() {
            return Ok(runtime);
        }
    }

    let source = match source_override.cloned() {
        Some(source) => source,
        None => managed_avf_linux_guest_source().ok_or_else(|| {
            anyhow::anyhow!(
                "managed AVF Linux guest runtime source is not available for {}/{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?,
    };
    let runtime = AvfLinuxGuestRuntime::from_source(data_root, &source)?;
    if avf_linux_runtime_is_ready(&runtime) {
        return Ok(runtime);
    }

    let install_lock = managed_avf_linux_install_lock();
    let lock_wait_started = Instant::now();
    let (waited_for_existing_install, install_guard) = match install_lock.try_lock() {
        Ok(guard) => {
            observe_phase(
                observer,
                HarnessSetupPhase::ArtifactDownload,
                "preparing managed AVF Linux guest runtime artifacts",
            );
            (false, guard)
        }
        Err(_) => {
            observe_phase(
                observer,
                HarnessSetupPhase::ArtifactDownload,
                "waiting for managed AVF Linux guest runtime preparation",
            );
            observe_log(
                observer,
                HarnessSetupPhase::ArtifactDownload,
                HarnessSetupLogLevel::Info,
                "waiting for another launch to finish preparing the managed AVF Linux guest runtime",
            );
            (true, install_lock.lock().await)
        }
    };
    let _install_guard = install_guard;
    if waited_for_existing_install {
        emit_runtime_install_info(
            observer,
            &format!(
                "shared AVF Linux runtime preparation wait finished in {} ms",
                lock_wait_started.elapsed().as_millis()
            ),
        );
    }
    let runtime = AvfLinuxGuestRuntime::from_source(data_root, &source)?;
    if avf_linux_runtime_is_ready(&runtime) {
        if waited_for_existing_install {
            emit_runtime_install_info(
                observer,
                "managed AVF Linux guest runtime became ready while waiting for shared preparation",
            );
        }
        return Ok(runtime);
    }

    emit_runtime_install_info(
        observer,
        &format!(
            "installing managed AVF Linux guest runtime {}",
            runtime.version
        ),
    );

    let final_archive = managed_avf_linux_archive_path(data_root, &source);
    let archive_lock = managed_artifact_lock_path(&final_archive);
    let _archive_guard = acquire_managed_artifact_file_lock(
        &archive_lock,
        "AVF Linux guest runtime archive",
        observer,
        HarnessSetupPhase::ArtifactDownload,
    )
    .await?;
    let partial_archive = managed_artifact_partial_path(&final_archive);
    if final_archive.exists() {
        let digest = sha256_hex_file(&final_archive)
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
            bail!(
                "managed AVF Linux archive path has no parent: {}",
                final_archive.display()
            );
        };
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
        let archive_download_started = Instant::now();
        emit_runtime_install_info(
            observer,
            &format!(
                "starting managed AVF Linux runtime archive download for {}",
                runtime.version
            ),
        );
        download_managed_artifact(
            &source.uri,
            &partial_archive,
            Some(ManagedArtifactDownloadReporter::new(
                observer,
                download_aggregate.clone(),
                HarnessSetupPhase::ArtifactDownload,
                AVF_LINUX_ROOTFS_LABEL,
            )),
        )
        .await?;
        finalize_managed_artifact_download(
            &partial_archive,
            &final_archive,
            &source.sha256,
            "managed AVF Linux guest runtime archive",
        )
        .await?;
        emit_runtime_install_info(
            observer,
            &format!(
                "managed AVF Linux runtime archive download finished in {} ms",
                archive_download_started.elapsed().as_millis()
            ),
        );
    }

    let Some(parent) = runtime.runtime_root.parent() else {
        bail!(
            "managed AVF Linux runtime root has no parent: {}",
            runtime.runtime_root.display()
        );
    };
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating {}", parent.display()))?;
    let staging_dir = parent.join(format!(
        ".avf-linux-staging-{}",
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
    let archive_for_extract = final_archive.clone();
    let uri_for_extract = source.uri.clone();
    let extract_dir_for_extract = extract_dir.clone();
    let extract_started = Instant::now();
    emit_runtime_install_info(
        observer,
        &format!(
            "extracting managed AVF Linux runtime archive for {}",
            runtime.version
        ),
    );
    tokio::task::spawn_blocking(move || {
        extract_archive_to_dir(
            &archive_for_extract,
            &uri_for_extract,
            &extract_dir_for_extract,
        )
    })
    .await
    .context("joining managed AVF Linux extract task")??;
    let extracted_root = tokio::task::spawn_blocking({
        let extract_dir = extract_dir.clone();
        move || resolve_single_extracted_root(&extract_dir)
    })
    .await
    .context("joining managed AVF Linux extraction root task")??;
    emit_runtime_install_info(
        observer,
        &format!(
            "managed AVF Linux runtime archive extract finished in {} ms",
            extract_started.elapsed().as_millis()
        ),
    );

    let materialize_started = Instant::now();
    emit_runtime_install_info(
        observer,
        &format!(
            "materializing managed AVF Linux runtime {} into place",
            runtime.version
        ),
    );
    if runtime.runtime_root.exists() {
        let _ = fs::remove_dir_all(&runtime.runtime_root).await;
    }
    fs::rename(&extracted_root, &runtime.runtime_root)
        .await
        .with_context(|| {
            format!(
                "moving extracted AVF Linux runtime into place: {} -> {}",
                extracted_root.display(),
                runtime.runtime_root.display()
            )
        })?;
    let _ = fs::remove_dir_all(&staging_dir).await;
    emit_runtime_install_info(
        observer,
        &format!(
            "managed AVF Linux runtime materialization finished in {} ms",
            materialize_started.elapsed().as_millis()
        ),
    );

    let mut helper_downloads = Vec::new();
    let helper_downloads_started = Instant::now();
    emit_runtime_install_info(
        observer,
        &format!(
            "verifying and downloading managed AVF Linux helpers for {}",
            runtime.version
        ),
    );
    for (helper_name, label) in [
        (AVF_LINUX_KERNEL_HELPER, "Linux kernel"),
        (AVF_LINUX_INITRD_HELPER, "Linux initrd"),
        (AVF_LINUX_GUEST_AGENT_HELPER, "Guest agent"),
        (AVF_LINUX_EGRESS_PROXY_HELPER, "Egress proxy"),
        (AVF_LINUX_CONTAINER_STACK_HELPER, "Guest container stack"),
    ] {
        let helper_source = source.helpers.get(helper_name).cloned().ok_or_else(|| {
            anyhow::anyhow!("managed AVF Linux runtime is missing helper '{helper_name}'")
        })?;
        let helper_root = runtime.runtime_root.clone();
        let aggregate = download_aggregate.clone();
        helper_downloads.push(async move {
            let helper_path =
                managed_avf_linux_helper_path(&helper_root, helper_name).ok_or_else(|| {
                    anyhow::anyhow!("invalid AVF Linux helper path for '{helper_name}'")
                })?;
            let helper_lock = managed_artifact_lock_path(&helper_path);
            let _guard = acquire_managed_artifact_file_lock(
                &helper_lock,
                label,
                observer,
                HarnessSetupPhase::ArtifactDownload,
            )
            .await?;
            if let Some(parent) = helper_path.parent() {
                fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            if helper_path.exists() {
                let digest = sha256_hex_file(&helper_path)
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
                    label,
                )),
            )
            .await?;
            finalize_managed_artifact_download(&tmp, &helper_path, &helper_source.sha256, label)
                .await?;
            Ok(())
        });
    }
    for result in futures::future::join_all(helper_downloads).await {
        result?;
    }
    emit_runtime_install_info(
        observer,
        &format!(
            "managed AVF Linux helper verification/download finished in {} ms",
            helper_downloads_started.elapsed().as_millis()
        ),
    );

    if !runtime.rootfs_image.exists() {
        bail!(
            "managed AVF Linux runtime installed but rootfs image is missing at {}",
            runtime.rootfs_image.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for path in [&runtime.kernel_path, &runtime.initrd_path] {
            if path.exists() {
                let mut perms = fs::metadata(path)
                    .await
                    .with_context(|| format!("metadata {}", path.display()))?
                    .permissions();
                perms.set_mode(0o644);
                fs::set_permissions(path, perms)
                    .await
                    .with_context(|| format!("chmod {}", path.display()))?;
            }
        }
        for path in [
            runtime.guest_agent_path.as_ref(),
            runtime.egress_proxy_path.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if path.exists() {
                let mut perms = fs::metadata(path)
                    .await
                    .with_context(|| format!("metadata {}", path.display()))?
                    .permissions();
                perms.set_mode(0o755);
                fs::set_permissions(path, perms)
                    .await
                    .with_context(|| format!("chmod {}", path.display()))?;
            }
        }
        if runtime.container_stack_path.exists() {
            let mut perms = fs::metadata(&runtime.container_stack_path)
                .await
                .with_context(|| format!("metadata {}", runtime.container_stack_path.display()))?
                .permissions();
            perms.set_mode(0o644);
            fs::set_permissions(&runtime.container_stack_path, perms)
                .await
                .with_context(|| format!("chmod {}", runtime.container_stack_path.display()))?;
        }
    }
    mark_managed_avf_linux_runtime_ready(&runtime.runtime_root, &source).await?;
    emit_runtime_install_info(
        observer,
        &format!(
            "managed AVF Linux guest runtime {} is fully ready after {} ms total",
            runtime.version,
            install_started.elapsed().as_millis()
        ),
    );
    Ok(runtime)
}

#[cfg(any(test, feature = "test-support"))]
fn test_runtime_source_override() -> &'static StdMutex<Option<bundled_assets::ManagedRuntimeSource>>
{
    static OVERRIDE: OnceLock<StdMutex<Option<bundled_assets::ManagedRuntimeSource>>> =
        OnceLock::new();
    OVERRIDE.get_or_init(|| StdMutex::new(None))
}

#[cfg(any(test, feature = "test-support"))]
fn lock_test_runtime_source_override(
) -> StdMutexGuard<'static, Option<bundled_assets::ManagedRuntimeSource>> {
    test_runtime_source_override()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestManagedAvfLinuxRuntimeSourceGuard {
    previous: Option<bundled_assets::ManagedRuntimeSource>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestManagedAvfLinuxRuntimeSourceGuard {
    fn drop(&mut self) {
        let mut guard = lock_test_runtime_source_override();
        *guard = self.previous.take();
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn override_managed_avf_linux_runtime_source_for_test(
    source: bundled_assets::ManagedRuntimeSource,
) -> TestManagedAvfLinuxRuntimeSourceGuard {
    let mut guard = lock_test_runtime_source_override();
    let previous = guard.replace(source);
    TestManagedAvfLinuxRuntimeSourceGuard { previous }
}
