use std::{
    fmt, fs,
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};

#[cfg(test)]
use serde_json::json;

use super::model_bundle::{
    cached_signed_bundle, create_signed_bundle_staging_directory,
    create_signed_bundle_staging_file, lock_signed_bundle_cache, prepare_signed_bundle_cache,
    publish_signed_bundle, remove_signed_bundle_staging_directory,
    repair_interrupted_signed_bundle_publication, signed_bundle_cache_error_kind,
    signed_bundle_cache_status, validate_bundle_contract, verify_model_bundle,
    SignedBundleCacheErrorKind, SignedBundleCacheStatus, VerifiedModelBundle, MAX_BUNDLE_BYTES,
};
use super::model_contract::{CoreMlBundleContract, COREML_BUNDLE_CONTRACT};
use crate::{ArtifactFetchRequest, ArtifactFetcher};

const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_EXPANDED_ARCHIVE_BYTES: u64 = MAX_BUNDLE_BYTES + 64 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

static ACQUISITION_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelAcquisitionErrorKind {
    Unavailable,
    Integrity,
}

#[derive(Debug)]
pub(crate) struct ModelAcquisitionError {
    kind: ModelAcquisitionErrorKind,
    message: String,
}

impl fmt::Display for ModelAcquisitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = match self.kind {
            ModelAcquisitionErrorKind::Unavailable => "unavailable",
            ModelAcquisitionErrorKind::Integrity => "integrity failure",
        };
        write!(formatter, "Core ML model {class}: {}", self.message)
    }
}

impl std::error::Error for ModelAcquisitionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreMlAcquisitionSource {
    Cache,
    Download,
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct AcquiredCoreMlBundle {
    pub bundle: VerifiedModelBundle,
    pub source: CoreMlAcquisitionSource,
}

pub(crate) fn model_acquisition_error_kind(
    error: &anyhow::Error,
) -> Option<ModelAcquisitionErrorKind> {
    error
        .downcast_ref::<ModelAcquisitionError>()
        .map(|error| error.kind)
}

pub(crate) fn model_acquisition_integrity_error(error: &anyhow::Error) -> bool {
    model_acquisition_error_kind(error) == Some(ModelAcquisitionErrorKind::Integrity)
}

pub(crate) fn coreml_descriptor_provisioned() -> bool {
    descriptor_provisioned(&COREML_BUNDLE_CONTRACT)
}

pub(crate) fn coreml_bundle_cache_available(cache_root: &Path) -> bool {
    descriptor_cache_complete(cache_root, &COREML_BUNDLE_CONTRACT)
}

pub(crate) fn cached_coreml_bundle(cache_root: &Path) -> Result<Option<VerifiedModelBundle>> {
    cached_coreml_bundle_for(cache_root, &COREML_BUNDLE_CONTRACT)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn acquire_coreml_bundle_for_daemon(
    cache_root: &Path,
    artifact_fetcher: &dyn ArtifactFetcher,
) -> Result<AcquiredCoreMlBundle> {
    acquire_coreml_bundle_for(cache_root, &COREML_BUNDLE_CONTRACT, artifact_fetcher)
}

fn descriptor_provisioned(descriptor: &CoreMlBundleContract<'_>) -> bool {
    descriptor.provisioned()
}

fn validate_descriptor(descriptor: &CoreMlBundleContract<'_>) -> Result<()> {
    if !descriptor_provisioned(descriptor) {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            "compiled bundle descriptor is awaiting final artifact hashes",
        ));
    }
    for (name, digest) in [
        ("archive", descriptor.archive_sha256),
        ("manifest", descriptor.manifest_sha256),
    ] {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Integrity,
                format!("compiled {name} SHA-256 is invalid"),
            ));
        }
    }
    if descriptor.schema_version != COREML_BUNDLE_CONTRACT.schema_version
        || descriptor.artifact_name.is_empty()
        || descriptor.artifact_name.contains('/')
        || !descriptor.artifact_name.ends_with(".tar.xz")
        || !descriptor.artifact_url.ends_with(descriptor.artifact_name)
    {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "compiled bundle descriptor is internally inconsistent",
        ));
    }
    Ok(())
}

fn descriptor_cache_complete(cache_root: &Path, descriptor: &CoreMlBundleContract<'_>) -> bool {
    if validate_descriptor(descriptor).is_err() {
        return false;
    }
    signed_bundle_cache_status(cache_root, descriptor) == SignedBundleCacheStatus::Available
}

fn cached_coreml_bundle_for(
    cache_root: &Path,
    descriptor: &CoreMlBundleContract<'_>,
) -> Result<Option<VerifiedModelBundle>> {
    validate_descriptor(descriptor)?;
    ensure_macos_version_supported(descriptor.minimum_macos)?;
    cached_signed_bundle(cache_root, descriptor)
        .map_err(|error| acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string()))
}

fn acquire_coreml_bundle_for(
    cache_root: &Path,
    descriptor: &CoreMlBundleContract<'_>,
    artifact_fetcher: &dyn ArtifactFetcher,
) -> Result<AcquiredCoreMlBundle> {
    validate_descriptor(descriptor)?;
    ensure_macos_version_supported(descriptor.minimum_macos)?;
    let artifacts = prepare_signed_bundle_cache(cache_root)?;
    let _acquisition_lock =
        lock_signed_bundle_cache(&artifacts).map_err(map_signed_bundle_cache_error)?;

    repair_interrupted_signed_bundle_publication(cache_root, descriptor)
        .map_err(map_signed_bundle_cache_error)?;
    if let Some(bundle) = cached_coreml_bundle_for(cache_root, descriptor)? {
        return Ok(AcquiredCoreMlBundle {
            bundle,
            source: CoreMlAcquisitionSource::Cache,
        });
    }
    let archive_path = unique_child(&artifacts, "download", "tar.xz");
    let staging_path = unique_child(&artifacts, "extract", "bundle");

    let result = (|| -> Result<AcquiredCoreMlBundle> {
        let mut archive_file = create_signed_bundle_staging_file(&archive_path)
            .with_context(|| "create Core ML archive staging file")?;
        artifact_fetcher
            .fetch_to_writer(
                ArtifactFetchRequest::new(
                    descriptor.artifact_url,
                    MAX_ARCHIVE_BYTES,
                    DOWNLOAD_TIMEOUT,
                ),
                &mut archive_file,
            )
            .map_err(|error| {
                acquisition_error(
                    ModelAcquisitionErrorKind::Unavailable,
                    format!("artifact download failed: {error}"),
                )
            })?;
        archive_file
            .sync_all()
            .context("sync Core ML archive staging file")?;
        drop(archive_file);
        verify_archive_hash(&archive_path, descriptor.archive_sha256)?;

        create_signed_bundle_staging_directory(&staging_path)?;
        extract_archive(&archive_path, &staging_path, descriptor)?;
        let bundle = verify_descriptor_bundle(&staging_path, descriptor)?;
        publish_signed_bundle(cache_root, &staging_path, descriptor)
            .map_err(map_signed_bundle_cache_error)?;
        let installed = cached_coreml_bundle_for(cache_root, descriptor)?.ok_or_else(|| {
            acquisition_error(
                ModelAcquisitionErrorKind::Integrity,
                "installed bundle was not visible after atomic publication",
            )
        })?;
        debug_assert_eq!(bundle.manifest_sha256, installed.manifest_sha256);
        Ok(AcquiredCoreMlBundle {
            bundle: installed,
            source: CoreMlAcquisitionSource::Download,
        })
    })();

    let _ = fs::remove_file(&archive_path);
    remove_signed_bundle_staging_directory(&staging_path);
    result
}

mod archive;
use archive::{extract_archive, verify_archive_hash};

fn verify_descriptor_bundle(
    root: &Path,
    descriptor: &CoreMlBundleContract<'_>,
) -> Result<VerifiedModelBundle> {
    let bundle = verify_model_bundle(root).map_err(|error| {
        acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string())
    })?;
    validate_bundle_contract(&bundle, descriptor).map_err(|error| {
        acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string())
    })?;
    Ok(bundle)
}

fn map_signed_bundle_cache_error(error: anyhow::Error) -> anyhow::Error {
    let Some(kind) = signed_bundle_cache_error_kind(&error) else {
        return error;
    };
    let kind = match kind {
        SignedBundleCacheErrorKind::Unavailable => ModelAcquisitionErrorKind::Unavailable,
        SignedBundleCacheErrorKind::Integrity => ModelAcquisitionErrorKind::Integrity,
    };
    acquisition_error(kind, error.to_string())
}

fn unique_child(parent: &Path, purpose: &str, extension: &str) -> PathBuf {
    let nonce = ACQUISITION_NONCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".ctx-coreml-{purpose}-{}-{nonce}.{extension}",
        process::id()
    ))
}

fn acquisition_error(kind: ModelAcquisitionErrorKind, message: impl Into<String>) -> anyhow::Error {
    anyhow!(ModelAcquisitionError {
        kind,
        message: message.into(),
    })
}

fn archive_integrity(message: impl Into<String>) -> anyhow::Error {
    acquisition_error(ModelAcquisitionErrorKind::Integrity, message)
}

mod platform;
use platform::ensure_macos_version_supported;
#[cfg(test)]
use platform::version_at_least;

#[cfg(test)]
mod tests;
