use std::{
    fmt,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    manifest::{validate_bundle_contract, validate_sha256, VerifiedModelBundle},
    secure_fs::{
        create_new_nofollow, create_new_private_file, create_private_cache_root,
        create_private_directory_tree_nofollow, create_private_staging_directory,
        ensure_target_parent_inside_cache, metadata_if_present, reject_symlink_if_present,
        remove_real_directory_if_present, remove_real_file_if_present, require_real_directory,
        sync_parent, unique_sibling,
    },
    verify::{read_bounded_regular_file, verify_model_bundle},
};
use crate::semantic::model_contract::{CoreMlBundleContract, COREML_BUNDLE_CONTRACT};

pub(super) const CACHE_NAMESPACE: &str = "semantic-model-bundles";
pub(super) const COMPLETION_SUFFIX: &str = ".complete.json";
pub(super) const ARTIFACT_CACHE_DIR: &str = "semantic-model-artifacts";
const ACQUISITION_LOCK_FILE: &str = "acquisition.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CompletionMarker {
    schema_version: u32,
    manifest_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignedBundleCacheStatus {
    Missing,
    Available,
    IntegrityError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignedBundleCacheErrorKind {
    Unavailable,
    Integrity,
}

#[derive(Debug)]
struct SignedBundleCacheError {
    kind: SignedBundleCacheErrorKind,
    message: String,
}

impl fmt::Display for SignedBundleCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SignedBundleCacheError {}

pub(crate) fn signed_bundle_cache_error_kind(
    error: &anyhow::Error,
) -> Option<SignedBundleCacheErrorKind> {
    error
        .downcast_ref::<SignedBundleCacheError>()
        .map(|error| error.kind)
}

pub(crate) fn content_addressed_bundle_path(
    cache_root: &Path,
    manifest_sha256: &str,
) -> Result<PathBuf> {
    validate_sha256(manifest_sha256, "manifest_sha256")?;
    Ok(cache_root
        .join(CACHE_NAMESPACE)
        .join("sha256")
        .join(&manifest_sha256[..2])
        .join(manifest_sha256))
}

pub(crate) fn completion_marker_path(bundle_cache_path: &Path) -> Result<PathBuf> {
    let name = bundle_cache_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("bundle cache path has no UTF-8 file name"))?;
    validate_sha256(name, "bundle cache directory name")?;
    Ok(bundle_cache_path.with_file_name(format!("{name}{COMPLETION_SUFFIX}")))
}

pub(crate) fn completion_marker_matches(
    bundle_cache_path: &Path,
    manifest_sha256: &str,
) -> Result<bool> {
    validate_sha256(manifest_sha256, "manifest_sha256")?;
    let marker_path = completion_marker_path(bundle_cache_path)?;
    let bytes = match read_bounded_regular_file(&marker_path, 4096) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let marker: CompletionMarker = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse completion marker {}", marker_path.display()))?;
    Ok(
        marker.schema_version == COREML_BUNDLE_CONTRACT.schema_version
            && marker.manifest_sha256 == manifest_sha256,
    )
}

pub(crate) fn write_completion_marker_atomic(
    bundle_cache_path: &Path,
) -> Result<VerifiedModelBundle> {
    let verified = verify_model_bundle(bundle_cache_path)?;
    let manifest_sha256 = verified.manifest_sha256.as_str();
    let directory_hash = bundle_cache_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("bundle cache path has no UTF-8 file name"))?;
    if directory_hash != manifest_sha256 {
        bail!("bundle cache directory does not match manifest SHA-256");
    }
    require_real_directory(bundle_cache_path, "completed bundle cache directory")?;
    if completion_marker_matches(bundle_cache_path, manifest_sha256)? {
        return Ok(verified);
    }

    let marker_path = completion_marker_path(bundle_cache_path)?;
    reject_symlink_if_present(&marker_path)?;
    let temporary = unique_sibling(&marker_path, "marker")?;
    let marker = CompletionMarker {
        schema_version: COREML_BUNDLE_CONTRACT.schema_version,
        manifest_sha256: manifest_sha256.to_owned(),
    };
    let mut body = serde_json::to_vec(&marker)?;
    body.push(b'\n');
    let result = (|| -> Result<()> {
        let mut file = create_new_nofollow(&temporary)?;
        file.write_all(&body)
            .with_context(|| format!("write completion marker {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync completion marker {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, &marker_path)
            .with_context(|| format!("publish completion marker {}", marker_path.display()))?;
        sync_parent(&marker_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(verified)
}

pub(crate) fn prepare_signed_bundle_cache(cache_root: &Path) -> Result<PathBuf> {
    create_private_cache_root(cache_root)?;
    let artifacts = cache_root.join(ARTIFACT_CACHE_DIR);
    create_private_directory_tree_nofollow(cache_root, &artifacts)?;
    Ok(artifacts)
}

pub(crate) fn create_signed_bundle_staging_file(path: &Path) -> io::Result<File> {
    create_new_private_file(path)
}

pub(crate) fn create_signed_bundle_staging_directory(path: &Path) -> Result<()> {
    create_private_staging_directory(path)
}

pub(crate) fn remove_signed_bundle_staging_directory(path: &Path) {
    let _ = remove_real_directory_if_present(path);
}

pub(crate) fn lock_signed_bundle_cache(artifacts: &Path) -> Result<File> {
    let lock_path = artifacts.join(ACQUISITION_LOCK_FILE);
    if let Some(metadata) = metadata_if_present(&lock_path).map_err(|error| {
        signed_cache_error(
            SignedBundleCacheErrorKind::Unavailable,
            format!(
                "inspect Core ML acquisition lock {}: {error}",
                lock_path.display()
            ),
        )
    })? {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(signed_cache_error(
                SignedBundleCacheErrorKind::Integrity,
                "Core ML acquisition lock has an unexpected filesystem type",
            ));
        }
    }

    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(&lock_path).map_err(|error| {
        signed_cache_error(
            SignedBundleCacheErrorKind::Unavailable,
            format!(
                "open Core ML acquisition lock {}: {error}",
                lock_path.display()
            ),
        )
    })?;
    let metadata = fs::symlink_metadata(&lock_path).map_err(|error| {
        signed_cache_error(
            SignedBundleCacheErrorKind::Unavailable,
            format!(
                "inspect Core ML acquisition lock {}: {error}",
                lock_path.display()
            ),
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(signed_cache_error(
            SignedBundleCacheErrorKind::Integrity,
            "Core ML acquisition lock has an unexpected filesystem type",
        ));
    }

    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            #[cfg(test)]
            notify_signed_bundle_cache_lock_contended();
            fs2::FileExt::lock_exclusive(&lock).map_err(|error| {
                signed_cache_error(
                    SignedBundleCacheErrorKind::Unavailable,
                    format!("lock Core ML acquisition {}: {error}", lock_path.display()),
                )
            })?;
        }
        Err(error) => {
            return Err(signed_cache_error(
                SignedBundleCacheErrorKind::Unavailable,
                format!("lock Core ML acquisition {}: {error}", lock_path.display()),
            ));
        }
    }
    Ok(lock)
}

#[cfg(test)]
std::thread_local! {
    static SIGNED_BUNDLE_CACHE_LOCK_CONTENDED_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_signed_bundle_cache_lock_contended_hook(hook: impl FnOnce() + 'static) {
    SIGNED_BUNDLE_CACHE_LOCK_CONTENDED_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn notify_signed_bundle_cache_lock_contended() {
    SIGNED_BUNDLE_CACHE_LOCK_CONTENDED_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

pub(crate) fn signed_bundle_cache_status(
    cache_root: &Path,
    contract: &CoreMlBundleContract<'_>,
) -> SignedBundleCacheStatus {
    let Ok(path) = content_addressed_bundle_path(cache_root, contract.manifest_sha256) else {
        return SignedBundleCacheStatus::IntegrityError;
    };
    let Ok(marker) = completion_marker_path(&path) else {
        return SignedBundleCacheStatus::IntegrityError;
    };
    let path_exists = fs::symlink_metadata(&path).is_ok();
    let marker_exists = fs::symlink_metadata(&marker).is_ok();
    match (path_exists, marker_exists) {
        (false, false) => SignedBundleCacheStatus::Missing,
        (true, true)
            if completion_marker_matches(&path, contract.manifest_sha256).unwrap_or(false) =>
        {
            SignedBundleCacheStatus::Available
        }
        _ => SignedBundleCacheStatus::IntegrityError,
    }
}

pub(crate) fn cached_signed_bundle(
    cache_root: &Path,
    contract: &CoreMlBundleContract<'_>,
) -> Result<Option<VerifiedModelBundle>> {
    let path = content_addressed_bundle_path(cache_root, contract.manifest_sha256)?;
    let marker = completion_marker_path(&path)?;
    let path_exists = fs::symlink_metadata(&path).is_ok();
    let marker_exists = fs::symlink_metadata(&marker).is_ok();
    if !path_exists && !marker_exists {
        return Ok(None);
    }
    if !path_exists || !marker_exists {
        bail!("content-addressed cache entry is incomplete");
    }
    if !completion_marker_matches(&path, contract.manifest_sha256)? {
        bail!("content-addressed cache completion marker does not match the descriptor");
    }
    let bundle = verify_model_bundle(&path)?;
    validate_bundle_contract(&bundle, contract)?;
    Ok(Some(bundle))
}

pub(crate) fn repair_interrupted_signed_bundle_publication(
    cache_root: &Path,
    contract: &CoreMlBundleContract<'_>,
) -> Result<()> {
    let path = content_addressed_bundle_path(cache_root, contract.manifest_sha256)
        .map_err(|error| signed_cache_integrity(error.to_string()))?;
    let marker =
        completion_marker_path(&path).map_err(|error| signed_cache_integrity(error.to_string()))?;
    let path_metadata = inspect_cache_path(&path)?;
    let marker_metadata = inspect_cache_path(&marker)?;

    match (path_metadata, marker_metadata) {
        (Some(metadata), None) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            ensure_repair_target(cache_root, &path)?;
            fs::remove_dir_all(&path).map_err(|error| {
                signed_cache_error(
                    SignedBundleCacheErrorKind::Unavailable,
                    format!(
                        "remove interrupted Core ML bundle publication {}: {error}",
                        path.display()
                    ),
                )
            })?;
        }
        (None, Some(metadata)) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            ensure_repair_target(cache_root, &marker)?;
            fs::remove_file(&marker).map_err(|error| {
                signed_cache_error(
                    SignedBundleCacheErrorKind::Unavailable,
                    format!(
                        "remove interrupted Core ML completion marker {}: {error}",
                        marker.display()
                    ),
                )
            })?;
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(signed_cache_integrity(
                "refusing to repair incomplete content-addressed cache entry with an unexpected filesystem type",
            ));
        }
        (None, None) | (Some(_), Some(_)) => {}
    }
    Ok(())
}

pub(crate) fn publish_signed_bundle(
    cache_root: &Path,
    staging: &Path,
    contract: &CoreMlBundleContract<'_>,
) -> Result<VerifiedModelBundle> {
    let staged = verify_model_bundle(staging)
        .and_then(|bundle| {
            validate_bundle_contract(&bundle, contract)?;
            Ok(bundle)
        })
        .map_err(|error| signed_cache_integrity(error.to_string()))?;
    let final_path = content_addressed_bundle_path(cache_root, contract.manifest_sha256)
        .map_err(|error| signed_cache_integrity(error.to_string()))?;
    let parent = final_path
        .parent()
        .ok_or_else(|| signed_cache_integrity("bundle cache path has no parent"))?;
    create_private_directory_tree_nofollow(cache_root, parent)?;
    reject_symlink_if_present(&final_path)
        .map_err(|error| signed_cache_integrity(error.to_string()))?;

    let installed = match fs::rename(staging, &final_path) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists || final_path.exists() => {
            let winner = verify_model_bundle(&final_path)
                .and_then(|bundle| {
                    validate_bundle_contract(&bundle, contract)?;
                    Ok(bundle)
                })
                .map_err(|error| signed_cache_integrity(error.to_string()))?;
            remove_real_directory_if_present(staging)?;
            debug_assert_eq!(winner.manifest_sha256, staged.manifest_sha256);
            false
        }
        Err(error) => {
            return Err(signed_cache_error(
                SignedBundleCacheErrorKind::Unavailable,
                format!("atomically publish model bundle: {error}"),
            ));
        }
    };

    match write_completion_marker_atomic(&final_path) {
        Ok(bundle) => {
            validate_bundle_contract(&bundle, contract)
                .map_err(|error| signed_cache_integrity(error.to_string()))?;
            Ok(bundle)
        }
        Err(error) if installed => {
            let rollback = rollback_signed_bundle_publication(&final_path);
            match rollback {
                Ok(()) => Err(signed_cache_integrity(error.to_string())),
                Err(rollback) => Err(signed_cache_integrity(format!(
                    "{error}; roll back signed bundle publication: {rollback}"
                ))),
            }
        }
        Err(error) => Err(signed_cache_integrity(error.to_string())),
    }
}

fn inspect_cache_path(path: &Path) -> Result<Option<fs::Metadata>> {
    metadata_if_present(path).map_err(|error| {
        signed_cache_error(
            SignedBundleCacheErrorKind::Unavailable,
            format!("inspect Core ML cache path {}: {error}", path.display()),
        )
    })
}

fn ensure_repair_target(cache_root: &Path, target: &Path) -> Result<()> {
    ensure_target_parent_inside_cache(cache_root, target)
        .map_err(|error| signed_cache_integrity(error.to_string()))
}

fn rollback_signed_bundle_publication(final_path: &Path) -> Result<()> {
    remove_real_directory_if_present(final_path)?;
    remove_real_file_if_present(&completion_marker_path(final_path)?)
}

fn signed_cache_integrity(message: impl Into<String>) -> anyhow::Error {
    signed_cache_error(SignedBundleCacheErrorKind::Integrity, message)
}

fn signed_cache_error(
    kind: SignedBundleCacheErrorKind,
    message: impl Into<String>,
) -> anyhow::Error {
    anyhow!(SignedBundleCacheError {
        kind,
        message: message.into(),
    })
}
