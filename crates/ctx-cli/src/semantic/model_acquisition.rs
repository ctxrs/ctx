use std::{
    collections::BTreeSet,
    fmt, fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::output::compact_json;

use super::model_bundle::{
    completion_marker_matches, completion_marker_path, content_addressed_bundle_path,
    validate_relative_path, verify_model_bundle, write_completion_marker_atomic,
    VerifiedModelBundle, MANIFEST_SCHEMA_VERSION, MAX_BUNDLE_BYTES, MAX_BUNDLE_DIRECTORIES,
    MAX_BUNDLE_FILES, MAX_FILE_BYTES,
};

const UNPROVISIONED_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_EXPANDED_ARCHIVE_BYTES: u64 = MAX_BUNDLE_BYTES + 64 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ARTIFACT_CACHE_DIR: &str = "semantic-model-artifacts";
const ACQUISITION_LOCK_FILE: &str = "acquisition.lock";

static ACQUISITION_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
pub(crate) struct CoreMlBundleDescriptor<'a> {
    pub artifact_url: &'a str,
    pub artifact_name: &'a str,
    pub archive_sha256: &'a str,
    pub manifest_sha256: &'a str,
    pub bundle_id: &'a str,
    pub bundle_version: &'a str,
    pub schema_version: u32,
    pub source_revision: &'a str,
    pub minimum_macos: &'a str,
    pub document_batch_size: u32,
    pub query_batch_size: Option<u32>,
    pub max_sequence_length: u32,
    pub embedding_dimensions: u32,
    pub document_prefix: &'a str,
    pub query_prefix: &'a str,
    pub pooling: &'a str,
    pub normalization: &'a str,
}

// Update these values together after producing and independently verifying the
// final deterministic archive. Zero hashes keep an unfinished descriptor inert.
pub(crate) const COREML_BUNDLE_DESCRIPTOR: CoreMlBundleDescriptor<'static> =
    CoreMlBundleDescriptor {
        artifact_url: "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
        artifact_name: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
        archive_sha256: "94c6fac5c4250079401d383adf1b10270fe5d370f2091dbad17bf4823222321e",
        manifest_sha256: "576c68756563333fdf442e6859f2392ca0065b09a2cb5d73983e30de75df1ad6",
        bundle_id: "ctx.multilingual-e5-small.coreml.fp16",
        bundle_version: "1.0.0",
        schema_version: MANIFEST_SCHEMA_VERSION,
        source_revision: "614241f622f53c4eeff9890bdc4f31cfecc418b3",
        minimum_macos: "13.0",
        document_batch_size: 16,
        query_batch_size: Some(1),
        max_sequence_length: 512,
        embedding_dimensions: 384,
        document_prefix: "passage: ",
        query_prefix: "query: ",
        pooling: "attention_mask_mean",
        normalization: "l2",
    };

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
    descriptor_provisioned(&COREML_BUNDLE_DESCRIPTOR)
}

pub(crate) fn coreml_bundle_cache_available(cache_root: &Path) -> bool {
    descriptor_cache_complete(cache_root, &COREML_BUNDLE_DESCRIPTOR)
}

pub(crate) fn coreml_acquisition_status_json(cache_root: &Path) -> Value {
    let descriptor = &COREML_BUNDLE_DESCRIPTOR;
    let provisioned = descriptor_provisioned(descriptor);
    let cache_status = if !provisioned {
        "descriptor_unprovisioned"
    } else {
        descriptor_cache_status(cache_root, descriptor)
    };
    compact_json(json!({
        "artifact_name": descriptor.artifact_name,
        "bundle_id": descriptor.bundle_id,
        "bundle_version": descriptor.bundle_version,
        "schema_version": descriptor.schema_version,
        "source_revision": descriptor.source_revision,
        "minimum_macos": descriptor.minimum_macos,
        "tensor_contract": {
            "document_batch_size": descriptor.document_batch_size,
            "query_batch_size": descriptor.query_batch_size,
            "max_sequence_length": descriptor.max_sequence_length,
            "embedding_dimensions": descriptor.embedding_dimensions,
            "document_prefix": descriptor.document_prefix,
            "query_prefix": descriptor.query_prefix,
            "pooling": descriptor.pooling,
            "normalization": descriptor.normalization,
        },
        "descriptor_provisioned": provisioned,
        "cache_status": cache_status,
        "network_scope": "daemon_only",
    }))
}

pub(crate) fn cached_coreml_bundle(cache_root: &Path) -> Result<Option<VerifiedModelBundle>> {
    cached_coreml_bundle_for(cache_root, &COREML_BUNDLE_DESCRIPTOR)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn acquire_coreml_bundle_for_daemon(cache_root: &Path) -> Result<AcquiredCoreMlBundle> {
    acquire_coreml_bundle_for(cache_root, &COREML_BUNDLE_DESCRIPTOR)
}

fn descriptor_provisioned(descriptor: &CoreMlBundleDescriptor<'_>) -> bool {
    descriptor.archive_sha256 != UNPROVISIONED_SHA256
        && descriptor.manifest_sha256 != UNPROVISIONED_SHA256
}

fn validate_descriptor(descriptor: &CoreMlBundleDescriptor<'_>) -> Result<()> {
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
    if descriptor.schema_version != MANIFEST_SCHEMA_VERSION
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

fn descriptor_bundle_path(
    cache_root: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> Result<PathBuf> {
    content_addressed_bundle_path(cache_root, descriptor.manifest_sha256)
        .map_err(|error| acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string()))
}

fn descriptor_cache_complete(cache_root: &Path, descriptor: &CoreMlBundleDescriptor<'_>) -> bool {
    if validate_descriptor(descriptor).is_err() {
        return false;
    }
    let Ok(path) = descriptor_bundle_path(cache_root, descriptor) else {
        return false;
    };
    path.is_dir() && completion_marker_matches(&path, descriptor.manifest_sha256).unwrap_or(false)
}

fn descriptor_cache_status(
    cache_root: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> &'static str {
    let Ok(path) = descriptor_bundle_path(cache_root, descriptor) else {
        return "integrity_error";
    };
    let Ok(marker) = completion_marker_path(&path) else {
        return "integrity_error";
    };
    let path_exists = fs::symlink_metadata(&path).is_ok();
    let marker_exists = fs::symlink_metadata(&marker).is_ok();
    match (path_exists, marker_exists) {
        (false, false) => "missing",
        (true, true)
            if completion_marker_matches(&path, descriptor.manifest_sha256).unwrap_or(false) =>
        {
            "available"
        }
        _ => "integrity_error",
    }
}

fn cached_coreml_bundle_for(
    cache_root: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> Result<Option<VerifiedModelBundle>> {
    validate_descriptor(descriptor)?;
    ensure_macos_version_supported(descriptor.minimum_macos)?;
    let path = descriptor_bundle_path(cache_root, descriptor)?;
    let marker = completion_marker_path(&path).map_err(|error| {
        acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string())
    })?;
    let path_exists = fs::symlink_metadata(&path).is_ok();
    let marker_exists = fs::symlink_metadata(&marker).is_ok();
    if !path_exists && !marker_exists {
        return Ok(None);
    }
    if !path_exists || !marker_exists {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "content-addressed cache entry is incomplete",
        ));
    }
    if !completion_marker_matches(&path, descriptor.manifest_sha256).map_err(|error| {
        acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string())
    })? {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "content-addressed cache completion marker does not match the descriptor",
        ));
    }
    let bundle = verify_descriptor_bundle(&path, descriptor)?;
    Ok(Some(bundle))
}

fn acquire_coreml_bundle_for(
    cache_root: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> Result<AcquiredCoreMlBundle> {
    validate_descriptor(descriptor)?;
    ensure_macos_version_supported(descriptor.minimum_macos)?;
    create_private_dir_all(cache_root)?;
    let artifacts = cache_root.join(ARTIFACT_CACHE_DIR);
    create_private_dir_all(&artifacts)?;
    let _acquisition_lock = lock_coreml_acquisition(&artifacts)?;

    repair_interrupted_cache_publication(cache_root, descriptor)?;
    if let Some(bundle) = cached_coreml_bundle_for(cache_root, descriptor)? {
        return Ok(AcquiredCoreMlBundle {
            bundle,
            source: CoreMlAcquisitionSource::Cache,
        });
    }
    let archive_path = unique_child(&artifacts, "download", "tar.xz");
    let staging_path = unique_child(&artifacts, "extract", "bundle");

    let result = (|| -> Result<AcquiredCoreMlBundle> {
        let mut archive_file = create_new_private_file(&archive_path)
            .with_context(|| "create Core ML archive staging file")?;
        crate::net::get_to_writer_limited(
            descriptor.artifact_url,
            MAX_ARCHIVE_BYTES,
            DOWNLOAD_TIMEOUT,
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

        fs::create_dir(&staging_path).context("create Core ML extraction staging directory")?;
        extract_archive(&archive_path, &staging_path, descriptor)?;
        let bundle = verify_descriptor_bundle(&staging_path, descriptor)?;
        let final_path = descriptor_bundle_path(cache_root, descriptor)?;
        install_bundle(&staging_path, &final_path, descriptor)?;
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
    remove_real_directory_if_present(&staging_path);
    result
}

fn repair_interrupted_cache_publication(
    cache_root: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> Result<()> {
    validate_descriptor(descriptor)?;
    let path = descriptor_bundle_path(cache_root, descriptor)?;
    let marker = completion_marker_path(&path).map_err(|error| {
        acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string())
    })?;
    let path_metadata = metadata_if_present(&path)?;
    let marker_metadata = metadata_if_present(&marker)?;

    match (path_metadata, marker_metadata) {
        (Some(metadata), None) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            ensure_repair_target_inside_cache(cache_root, &path)?;
            fs::remove_dir_all(&path).map_err(|error| {
                acquisition_error(
                    ModelAcquisitionErrorKind::Unavailable,
                    format!(
                        "remove interrupted Core ML bundle publication {}: {error}",
                        path.display()
                    ),
                )
            })?;
        }
        (None, Some(metadata)) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            ensure_repair_target_inside_cache(cache_root, &marker)?;
            fs::remove_file(&marker).map_err(|error| {
                acquisition_error(
                    ModelAcquisitionErrorKind::Unavailable,
                    format!(
                        "remove interrupted Core ML completion marker {}: {error}",
                        marker.display()
                    ),
                )
            })?;
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Integrity,
                "refusing to repair incomplete content-addressed cache entry with an unexpected filesystem type",
            ));
        }
        (None, None) | (Some(_), Some(_)) => {}
    }
    Ok(())
}

fn ensure_repair_target_inside_cache(cache_root: &Path, target: &Path) -> Result<()> {
    let canonical_root = fs::canonicalize(cache_root).map_err(|error| {
        acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            format!(
                "resolve Core ML cache root {} before repair: {error}",
                cache_root.display()
            ),
        )
    })?;
    let parent = target.parent().ok_or_else(|| {
        acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "Core ML cache repair target has no parent",
        )
    })?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| {
        acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            format!(
                "resolve Core ML cache repair parent {}: {error}",
                parent.display()
            ),
        )
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "refusing to repair a content-addressed cache entry outside the configured cache root",
        ));
    }
    Ok(())
}

fn metadata_if_present(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            format!("inspect Core ML cache path {}: {error}", path.display()),
        )),
    }
}

fn verify_archive_hash(path: &Path, expected: &str) -> Result<()> {
    let mut file = fs::File::open(path).context("open downloaded Core ML archive")?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).context("hash Core ML archive")?;
        if read == 0 {
            break;
        }
        count = count.saturating_add(read as u64);
        if count > MAX_ARCHIVE_BYTES {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Integrity,
                "downloaded archive exceeds compressed size limit",
            ));
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "downloaded archive SHA-256 does not match the compiled descriptor",
        ));
    }
    Ok(())
}

fn extract_archive(
    archive_path: &Path,
    destination: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> Result<()> {
    let file = fs::File::open(archive_path).context("open verified Core ML archive")?;
    let decoder = xz2::read::XzDecoder::new(file);
    let bounded = ExpandedReader::new(decoder, MAX_EXPANDED_ARCHIVE_BYTES);
    let mut archive = tar::Archive::new(bounded);
    let expected_root = descriptor
        .artifact_name
        .strip_suffix(".tar.xz")
        .ok_or_else(|| {
            acquisition_error(ModelAcquisitionErrorKind::Integrity, "invalid archive name")
        })?;
    let mut seen = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut files = 0_usize;
    let mut payload_bytes = 0_u64;
    let entries = archive.entries().map_err(archive_error)?;
    for item in entries {
        let mut entry = item.map_err(archive_error)?;
        let raw_path = entry.path().map_err(archive_error)?;
        let relative = archive_relative_path(&raw_path, expected_root)?;
        if relative.is_empty() {
            if !entry.header().entry_type().is_dir() || !seen.insert(String::new()) {
                return Err(archive_integrity(
                    "archive root entry is invalid or duplicated",
                ));
            }
            continue;
        }
        validate_relative_path(&relative).map_err(|error| archive_integrity(error.to_string()))?;
        if !seen.insert(relative.clone()) {
            return Err(archive_integrity("archive contains duplicate paths"));
        }
        let target = destination.join(&relative);
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            register_directory(destination, &target, &relative, &mut directories)?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(archive_integrity(
                "archive contains a link, device, sparse, or unknown entry type",
            ));
        }
        files += 1;
        if files > MAX_BUNDLE_FILES {
            return Err(archive_integrity("archive contains too many files"));
        }
        let size = entry.header().size().map_err(archive_error)?;
        if size > MAX_FILE_BYTES {
            return Err(archive_integrity(
                "archive member exceeds per-file size limit",
            ));
        }
        payload_bytes = payload_bytes
            .checked_add(size)
            .ok_or_else(|| archive_integrity("archive payload size overflow"))?;
        if payload_bytes > MAX_BUNDLE_BYTES {
            return Err(archive_integrity(
                "archive exceeds expanded payload size limit",
            ));
        }
        create_parent_directories(destination, &target, &relative, &mut directories)?;
        let mut output = create_new_private_file(&target)
            .map_err(|error| archive_integrity(format!("create extracted file: {error}")))?;
        let copied = copy_exact_limited(&mut entry, &mut output, size)?;
        if copied != size {
            return Err(archive_integrity(
                "archive member size does not match its header",
            ));
        }
        output
            .sync_all()
            .map_err(|error| archive_integrity(format!("sync extracted file: {error}")))?;
    }
    if files == 0 {
        return Err(archive_integrity("archive contains no payload files"));
    }
    Ok(())
}

fn archive_relative_path(path: &Path, expected_root: &str) -> Result<String> {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        return Err(archive_integrity(
            "archive path has no normal root component",
        ));
    };
    if root.to_str() != Some(expected_root) {
        return Err(archive_integrity(
            "archive path has an unexpected root directory",
        ));
    }
    let mut parts = Vec::new();
    for component in components {
        let Component::Normal(component) = component else {
            return Err(archive_integrity("archive path contains traversal"));
        };
        parts.push(
            component
                .to_str()
                .ok_or_else(|| archive_integrity("archive path is not UTF-8"))?,
        );
    }
    Ok(parts.join("/"))
}

fn register_directory(
    destination: &Path,
    target: &Path,
    relative: &str,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    create_parent_directories(destination, target, relative, directories)?;
    if directories.insert(relative.to_owned()) {
        if directories.len() > MAX_BUNDLE_DIRECTORIES {
            return Err(archive_integrity("archive contains too many directories"));
        }
        match fs::create_dir(target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && target.is_dir() => {}
            Err(error) => {
                return Err(archive_integrity(format!(
                    "create extracted directory: {error}"
                )))
            }
        }
    }
    Ok(())
}

fn create_parent_directories(
    destination: &Path,
    target: &Path,
    relative: &str,
    directories: &mut BTreeSet<String>,
) -> Result<()> {
    let Some(parent) = target.parent() else {
        return Err(archive_integrity("archive target has no parent"));
    };
    if parent == destination {
        return Ok(());
    }
    let mut current = destination.to_path_buf();
    let mut name = String::new();
    let parent_relative = Path::new(relative)
        .parent()
        .ok_or_else(|| archive_integrity("archive path parent is invalid"))?;
    for component in parent_relative.components() {
        let Component::Normal(component) = component else {
            return Err(archive_integrity("archive parent path contains traversal"));
        };
        let component = component
            .to_str()
            .ok_or_else(|| archive_integrity("archive parent path is not UTF-8"))?;
        if !name.is_empty() {
            name.push('/');
        }
        name.push_str(component);
        current.push(component);
        if directories.insert(name.clone()) {
            if directories.len() > MAX_BUNDLE_DIRECTORIES {
                return Err(archive_integrity("archive contains too many directories"));
            }
            fs::create_dir(&current).map_err(|error| {
                archive_integrity(format!("create extracted parent directory: {error}"))
            })?;
        } else if !current.is_dir() {
            return Err(archive_integrity(
                "archive path collides with a non-directory entry",
            ));
        }
    }
    Ok(())
}

fn copy_exact_limited(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected: u64,
) -> Result<u64> {
    let mut limited = reader.take(expected.saturating_add(1));
    let copied = io::copy(&mut limited, writer)
        .map_err(|error| archive_integrity(format!("extract archive member: {error}")))?;
    if copied > expected {
        return Err(archive_integrity(
            "archive member exceeds its declared size",
        ));
    }
    Ok(copied)
}

fn verify_descriptor_bundle(
    root: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> Result<VerifiedModelBundle> {
    let bundle = verify_model_bundle(root).map_err(|error| {
        acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string())
    })?;
    if bundle.manifest_sha256 != descriptor.manifest_sha256
        || bundle.manifest.schema_version != descriptor.schema_version
        || bundle.manifest.bundle_id != descriptor.bundle_id
        || bundle.manifest.bundle_version != descriptor.bundle_version
        || bundle.manifest.model.source_revision != descriptor.source_revision
        || bundle.manifest.tensor_contract.document_batch_size != descriptor.document_batch_size
        || bundle.manifest.tensor_contract.query_batch_size != descriptor.query_batch_size
        || bundle.manifest.tensor_contract.max_sequence_length != descriptor.max_sequence_length
        || bundle.manifest.tensor_contract.embedding_dimensions != descriptor.embedding_dimensions
        || bundle.manifest.tensor_contract.document_prefix != descriptor.document_prefix
        || bundle.manifest.tensor_contract.query_prefix != descriptor.query_prefix
        || bundle.manifest.tensor_contract.pooling != descriptor.pooling
        || bundle.manifest.tensor_contract.normalization != descriptor.normalization
    {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "bundle manifest does not match the compiled descriptor",
        ));
    }
    Ok(bundle)
}

fn install_bundle(
    staging: &Path,
    final_path: &Path,
    descriptor: &CoreMlBundleDescriptor<'_>,
) -> Result<()> {
    let parent = final_path
        .parent()
        .ok_or_else(|| archive_integrity("bundle cache path has no parent"))?;
    create_private_dir_all(parent)?;
    match fs::rename(staging, final_path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists || final_path.exists() => {
            verify_descriptor_bundle(final_path, descriptor)?;
            remove_real_directory_if_present(staging);
        }
        Err(error) => {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Unavailable,
                format!("atomically publish model bundle: {error}"),
            ))
        }
    }
    write_completion_marker_atomic(final_path).map_err(|error| {
        acquisition_error(ModelAcquisitionErrorKind::Integrity, error.to_string())
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_macos_version_supported(minimum: &str) -> Result<()> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map_err(|error| {
            acquisition_error(
                ModelAcquisitionErrorKind::Unavailable,
                format!("could not determine macOS version: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            "could not determine macOS version",
        ));
    }
    let actual = String::from_utf8_lossy(&output.stdout);
    if !version_at_least(actual.trim(), minimum)? {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            format!("requires macOS {minimum} or newer"),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_macos_version_supported(_minimum: &str) -> Result<()> {
    #[cfg(test)]
    return Ok(());
    #[cfg(not(test))]
    Err(acquisition_error(
        ModelAcquisitionErrorKind::Unavailable,
        "Core ML requires macOS",
    ))
}

fn version_at_least(actual: &str, minimum: &str) -> Result<bool> {
    fn parse(value: &str) -> Result<Vec<u64>> {
        let parts = value
            .split('.')
            .map(|part| part.parse::<u64>())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| {
                acquisition_error(
                    ModelAcquisitionErrorKind::Unavailable,
                    "macOS version has invalid syntax",
                )
            })?;
        if parts.is_empty() || parts.len() > 3 {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Unavailable,
                "macOS version has invalid syntax",
            ));
        }
        Ok(parts)
    }
    let mut actual = parse(actual)?;
    let mut minimum = parse(minimum)?;
    actual.resize(3, 0);
    minimum.resize(3, 0);
    Ok(actual >= minimum)
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create model cache directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_new_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn lock_coreml_acquisition(artifacts: &Path) -> Result<fs::File> {
    let lock_path = artifacts.join(ACQUISITION_LOCK_FILE);
    if let Some(metadata) = metadata_if_present(&lock_path)? {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Integrity,
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
        acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            format!(
                "open Core ML acquisition lock {}: {error}",
                lock_path.display()
            ),
        )
    })?;
    let metadata = fs::symlink_metadata(&lock_path).map_err(|error| {
        acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            format!(
                "inspect Core ML acquisition lock {}: {error}",
                lock_path.display()
            ),
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Integrity,
            "Core ML acquisition lock has an unexpected filesystem type",
        ));
    }

    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            #[cfg(test)]
            notify_acquisition_lock_contended();
            fs2::FileExt::lock_exclusive(&lock).map_err(|error| {
                acquisition_error(
                    ModelAcquisitionErrorKind::Unavailable,
                    format!("lock Core ML acquisition {}: {error}", lock_path.display()),
                )
            })?;
        }
        Err(error) => {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Unavailable,
                format!("lock Core ML acquisition {}: {error}", lock_path.display()),
            ));
        }
    }
    Ok(lock)
}

#[cfg(test)]
std::thread_local! {
    static ACQUISITION_LOCK_CONTENDED_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
fn set_acquisition_lock_contended_hook(hook: impl FnOnce() + 'static) {
    ACQUISITION_LOCK_CONTENDED_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn notify_acquisition_lock_contended() {
    ACQUISITION_LOCK_CONTENDED_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn unique_child(parent: &Path, purpose: &str, extension: &str) -> PathBuf {
    let nonce = ACQUISITION_NONCE.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(
        ".ctx-coreml-{purpose}-{}-{nonce}.{extension}",
        process::id()
    ))
}

fn remove_real_directory_if_present(path: &Path) {
    if fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        let _ = fs::remove_dir_all(path);
    }
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

fn archive_error(error: io::Error) -> anyhow::Error {
    archive_integrity(format!("read compressed archive: {error}"))
}

struct ExpandedReader<R> {
    inner: R,
    read: u64,
    maximum: u64,
}

impl<R> ExpandedReader<R> {
    fn new(inner: R, maximum: u64) -> Self {
        Self {
            inner,
            read: 0,
            maximum,
        }
    }
}

impl<R: Read> Read for ExpandedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.read = self.read.saturating_add(count as u64);
        if self.read > self.maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expanded archive exceeds size limit",
            ));
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn production_descriptor_is_hash_pinned_and_cache_probe_is_offline() {
        assert!(coreml_descriptor_provisioned());
        assert_eq!(COREML_BUNDLE_DESCRIPTOR.document_batch_size, 16);
        assert_eq!(COREML_BUNDLE_DESCRIPTOR.query_batch_size, Some(1));
        let temp = tempfile::tempdir().unwrap();
        assert!(cached_coreml_bundle(temp.path()).unwrap().is_none());
    }

    #[test]
    fn cache_only_probe_never_reads_artifact_url() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp
            .path()
            .join("ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz");
        let artifact_url = format!("file://{}", missing.display());
        let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);
        assert!(cached_coreml_bundle_for(temp.path(), &descriptor)
            .unwrap()
            .is_none());
        assert!(!missing.exists());
    }

    #[test]
    fn archive_hash_mismatch_is_an_integrity_failure() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp
            .path()
            .join("ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz");
        fs::write(&archive, b"not an archive").unwrap();
        let artifact_url = format!("file://{}", archive.display());
        let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);
        let error = acquire_coreml_bundle_for(temp.path(), &descriptor).unwrap_err();
        assert!(model_acquisition_integrity_error(&error));
        assert!(format!("{error:#}").contains("SHA-256"));
    }

    #[test]
    fn archive_paths_and_entry_types_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        for (path, entry_type) in [
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/../escape",
                tar::EntryType::Regular,
            ),
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/link",
                tar::EntryType::Symlink,
            ),
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/device",
                tar::EntryType::Char,
            ),
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/hardlink",
                tar::EntryType::Link,
            ),
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/fifo",
                tar::EntryType::Fifo,
            ),
            (
                "ctx-multilingual-e5-small-coreml-fp16-1.0.0/unknown",
                tar::EntryType::new(b'Z'),
            ),
        ] {
            let archive = temp
                .path()
                .join(format!("{}.tar.xz", path.rsplit('/').next().unwrap()));
            write_test_archive(&archive, &[(path, entry_type, b"x")]);
            let output = temp.path().join(format!("out-{}", entry_type.as_byte()));
            fs::create_dir(&output).unwrap();
            let descriptor = test_descriptor("file:///unused", A_HASH, B_HASH);
            let error = extract_archive(&archive, &output, &descriptor).unwrap_err();
            assert!(model_acquisition_integrity_error(&error));
        }
    }

    #[test]
    fn archive_duplicate_paths_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("duplicate.tar.xz");
        write_test_archive(
            &archive,
            &[
                (
                    "ctx-multilingual-e5-small-coreml-fp16-1.0.0/file",
                    tar::EntryType::Regular,
                    b"a",
                ),
                (
                    "ctx-multilingual-e5-small-coreml-fp16-1.0.0/file",
                    tar::EntryType::Regular,
                    b"b",
                ),
            ],
        );
        let output = temp.path().join("output");
        fs::create_dir(&output).unwrap();
        let descriptor = test_descriptor("file:///unused", A_HASH, B_HASH);
        let error = extract_archive(&archive, &output, &descriptor).unwrap_err();
        assert!(format!("{error:#}").contains("duplicate"));
    }

    #[test]
    fn macos_versions_compare_numerically() {
        assert!(version_at_least("14.7.5", "13.0").unwrap());
        assert!(version_at_least("13.0", "13.0").unwrap());
        assert!(!version_at_least("12.6.9", "13.0").unwrap());
        assert!(version_at_least("13.0.1", "13.0").unwrap());
    }

    #[test]
    fn verified_archive_installs_content_addressed_and_then_uses_cache_only() {
        let temp = tempfile::tempdir().unwrap();
        let (archive_path, archive_sha256, manifest_sha256) =
            create_test_bundle_archive(temp.path());
        let artifact_url = format!("file://{}", archive_path.display());
        let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);
        let cache = temp.path().join("cache");

        let acquired = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
        assert_eq!(acquired.source, CoreMlAcquisitionSource::Download);
        assert_eq!(acquired.bundle.manifest_sha256, manifest_sha256);
        let installed = descriptor_bundle_path(&cache, &descriptor).unwrap();
        assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());

        fs::remove_file(&archive_path).unwrap();
        let cached = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
        assert_eq!(cached.source, CoreMlAcquisitionSource::Cache);
        assert_eq!(cached.bundle.manifest_sha256, manifest_sha256);
    }

    #[test]
    fn acquisition_lock_keeps_repair_away_from_active_publication() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let manifest_sha256 = create_test_bundle(&source);
        let missing_archive = temp.path().join(COREML_BUNDLE_DESCRIPTOR.artifact_name);
        let artifact_url = format!("file://{}", missing_archive.display());
        let descriptor = test_descriptor(&artifact_url, A_HASH, &manifest_sha256);
        let cache = temp.path().join("cache");
        let artifacts = cache.join(ARTIFACT_CACHE_DIR);
        create_private_dir_all(&artifacts).unwrap();

        let publisher_lock = lock_coreml_acquisition(&artifacts).unwrap();
        let installed = descriptor_bundle_path(&cache, &descriptor).unwrap();
        create_private_dir_all(installed.parent().unwrap()).unwrap();
        fs::rename(&source, &installed).unwrap();
        let marker = completion_marker_path(&installed).unwrap();
        assert!(installed.is_dir());
        assert!(!marker.exists());

        let (contended_tx, contended_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let second = scope.spawn(|| {
                set_acquisition_lock_contended_hook(move || {
                    contended_tx.send(()).unwrap();
                });
                acquire_coreml_bundle_for(&cache, &descriptor)
            });

            if let Err(error) = contended_rx.recv_timeout(Duration::from_secs(5)) {
                write_completion_marker_atomic(&installed).unwrap();
                drop(publisher_lock);
                let second_result = second.join();
                panic!(
                    "second acquirer did not report lock contention: {error}; result: {second_result:?}"
                );
            }
            assert!(installed.is_dir());
            assert!(!marker.exists());

            write_completion_marker_atomic(&installed).unwrap();
            drop(publisher_lock);

            let acquired = second.join().unwrap().unwrap();
            assert_eq!(acquired.source, CoreMlAcquisitionSource::Cache);
            assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());
        });
    }

    #[test]
    fn daemon_acquisition_repairs_bundle_published_without_marker() {
        let temp = tempfile::tempdir().unwrap();
        let (archive_path, archive_sha256, manifest_sha256) =
            create_test_bundle_archive(temp.path());
        let artifact_url = format!("file://{}", archive_path.display());
        let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);
        let cache = temp.path().join("cache");
        acquire_coreml_bundle_for(&cache, &descriptor).unwrap();

        let installed = descriptor_bundle_path(&cache, &descriptor).unwrap();
        let marker = completion_marker_path(&installed).unwrap();
        fs::remove_file(marker).unwrap();
        fs::write(installed.join("interrupted-publication"), b"stale").unwrap();

        let repaired = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
        assert_eq!(repaired.source, CoreMlAcquisitionSource::Download);
        assert!(!installed.join("interrupted-publication").exists());
        assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());
    }

    #[test]
    fn daemon_acquisition_repairs_marker_published_without_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let (archive_path, archive_sha256, manifest_sha256) =
            create_test_bundle_archive(temp.path());
        let artifact_url = format!("file://{}", archive_path.display());
        let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);
        let cache = temp.path().join("cache");
        acquire_coreml_bundle_for(&cache, &descriptor).unwrap();

        let installed = descriptor_bundle_path(&cache, &descriptor).unwrap();
        let marker = completion_marker_path(&installed).unwrap();
        fs::remove_dir_all(&installed).unwrap();
        assert!(marker.is_file());

        let repaired = acquire_coreml_bundle_for(&cache, &descriptor).unwrap();
        assert_eq!(repaired.source, CoreMlAcquisitionSource::Download);
        assert!(completion_marker_matches(&installed, &manifest_sha256).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn daemon_acquisition_refuses_to_repair_symlinked_incomplete_entries() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let missing_archive = temp.path().join("missing.tar.xz");
        let artifact_url = format!("file://{}", missing_archive.display());
        let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);

        let directory_cache = temp.path().join("directory-cache");
        let directory_path = descriptor_bundle_path(&directory_cache, &descriptor).unwrap();
        fs::create_dir_all(directory_path.parent().unwrap()).unwrap();
        let directory_target = temp.path().join("directory-target");
        fs::create_dir(&directory_target).unwrap();
        fs::write(directory_target.join("keep"), b"keep").unwrap();
        symlink(&directory_target, &directory_path).unwrap();
        let error = acquire_coreml_bundle_for(&directory_cache, &descriptor).unwrap_err();
        assert!(model_acquisition_integrity_error(&error));
        assert_eq!(fs::read(directory_target.join("keep")).unwrap(), b"keep");

        let marker_cache = temp.path().join("marker-cache");
        let bundle_path = descriptor_bundle_path(&marker_cache, &descriptor).unwrap();
        fs::create_dir_all(bundle_path.parent().unwrap()).unwrap();
        let marker = completion_marker_path(&bundle_path).unwrap();
        let marker_target = temp.path().join("marker-target");
        fs::write(&marker_target, b"keep").unwrap();
        symlink(&marker_target, &marker).unwrap();
        let error = acquire_coreml_bundle_for(&marker_cache, &descriptor).unwrap_err();
        assert!(model_acquisition_integrity_error(&error));
        assert_eq!(fs::read(marker_target).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn daemon_acquisition_refuses_to_repair_through_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let missing_archive = temp.path().join("missing.tar.xz");
        let artifact_url = format!("file://{}", missing_archive.display());
        let descriptor = test_descriptor(&artifact_url, A_HASH, B_HASH);
        let cache = temp.path().join("cache");
        let installed = descriptor_bundle_path(&cache, &descriptor).unwrap();
        let digest_parent = installed.parent().unwrap();
        fs::create_dir_all(digest_parent.parent().unwrap()).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, digest_parent).unwrap();
        let outside_bundle = outside.join(installed.file_name().unwrap());
        fs::create_dir(&outside_bundle).unwrap();
        fs::write(outside_bundle.join("keep"), b"keep").unwrap();

        let error = acquire_coreml_bundle_for(&cache, &descriptor).unwrap_err();
        assert!(model_acquisition_integrity_error(&error));
        assert_eq!(fs::read(outside_bundle.join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn daemon_acquisition_does_not_repair_completed_integrity_failures() {
        let temp = tempfile::tempdir().unwrap();
        let (archive_path, archive_sha256, manifest_sha256) =
            create_test_bundle_archive(temp.path());
        let artifact_url = format!("file://{}", archive_path.display());
        let descriptor = test_descriptor(&artifact_url, &archive_sha256, &manifest_sha256);

        let marker_cache = temp.path().join("marker-cache");
        acquire_coreml_bundle_for(&marker_cache, &descriptor).unwrap();
        let marker_bundle = descriptor_bundle_path(&marker_cache, &descriptor).unwrap();
        let marker = completion_marker_path(&marker_bundle).unwrap();
        fs::write(
            &marker,
            format!(r#"{{"schema_version":1,"manifest_sha256":"{B_HASH}"}}"#),
        )
        .unwrap();
        let error = acquire_coreml_bundle_for(&marker_cache, &descriptor).unwrap_err();
        assert!(model_acquisition_integrity_error(&error));
        assert!(marker_bundle.is_dir());

        let content_cache = temp.path().join("content-cache");
        acquire_coreml_bundle_for(&content_cache, &descriptor).unwrap();
        let content_bundle = descriptor_bundle_path(&content_cache, &descriptor).unwrap();
        let model = content_bundle.join("document.mlpackage/Data/model.bin");
        fs::write(&model, b"tampered").unwrap();
        let error = acquire_coreml_bundle_for(&content_cache, &descriptor).unwrap_err();
        assert!(model_acquisition_integrity_error(&error));
        assert_eq!(fs::read(model).unwrap(), b"tampered");
        assert!(completion_marker_matches(&content_bundle, &manifest_sha256).unwrap());
    }

    fn test_descriptor<'a>(
        artifact_url: &'a str,
        archive_sha256: &'a str,
        manifest_sha256: &'a str,
    ) -> CoreMlBundleDescriptor<'a> {
        CoreMlBundleDescriptor {
            artifact_url,
            artifact_name: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
            archive_sha256,
            manifest_sha256,
            query_batch_size: None,
            ..COREML_BUNDLE_DESCRIPTOR
        }
    }

    fn write_test_archive(path: &Path, entries: &[(&str, tar::EntryType, &[u8])]) {
        let file = fs::File::create(path).unwrap();
        let encoder = xz2::write::XzEncoder::new(file, 1);
        let mut archive = tar::Builder::new(encoder);
        for (path, entry_type, body) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(*entry_type);
            header.set_mode(0o600);
            header.set_size(body.len() as u64);
            let name = path.as_bytes();
            assert!(name.len() < 100);
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name);
            header.set_cksum();
            archive.append(&header, *body).unwrap();
        }
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();
    }

    fn create_test_bundle_archive(root: &Path) -> (PathBuf, String, String) {
        let root_name = "ctx-multilingual-e5-small-coreml-fp16-1.0.0";
        let source = root.join(root_name);
        fs::create_dir(&source).unwrap();
        let manifest_sha256 = create_test_bundle(&source);
        let archive_path = root.join(format!("{root_name}.tar.xz"));
        write_bundle_archive(&archive_path, &source, root_name);
        let archive_sha256 = sha256_path(&archive_path);
        (archive_path, archive_sha256, manifest_sha256)
    }

    fn create_test_bundle(root: &Path) -> String {
        let payloads = [
            ("LICENSES/MODEL_LICENSE.txt", b"license\n".as_slice()),
            ("PROVENANCE.json", b"{}".as_slice()),
            ("THIRD_PARTY_NOTICES.md", b"notices\n".as_slice()),
            ("document.mlpackage/Data/model.bin", b"model".as_slice()),
            ("tokenizer.json", b"{}".as_slice()),
        ];
        let mut files = Vec::new();
        for (relative, body) in payloads {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            files.push(json!({
                "path": relative,
                "size_bytes": body.len(),
                "sha256": format!("{:x}", Sha256::digest(body)),
            }));
        }
        files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
        let manifest = json!({
            "schema_version": 1,
            "bundle_id": "ctx.multilingual-e5-small.coreml.fp16",
            "bundle_version": "1.0.0",
            "model": {
                "id": "intfloat/multilingual-e5-small",
                "source_revision": "614241f622f53c4eeff9890bdc4f31cfecc418b3",
                "embedding_space_id": "e5-small-v1:mean-pool:l2:query-passage",
                "precision": "fp16",
            },
            "tensor_contract": {
                "inputs": [
                    {"name": "input_ids", "dtype": "int32", "shape": [16, 512]},
                    {"name": "attention_mask", "dtype": "int32", "shape": [16, 512]},
                    {"name": "token_type_ids", "dtype": "int32", "shape": [16, 512]},
                ],
                "output": {"name": "sentence_embeddings", "dtype": "float32", "shape": [16, 384]},
                "document_batch_size": 16,
                "max_sequence_length": 512,
                "embedding_dimensions": 384,
                "document_prefix": "passage: ",
                "query_prefix": "query: ",
                "pooling": "attention_mask_mean",
                "normalization": "l2",
            },
            "artifacts": {
                "tokenizer": "tokenizer.json",
                "document_model": "document.mlpackage",
            },
            "files": files,
        });
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        fs::write(root.join("manifest.json"), &bytes).unwrap();
        format!("{:x}", Sha256::digest(bytes))
    }

    fn write_bundle_archive(path: &Path, root: &Path, root_name: &str) {
        let file = fs::File::create(path).unwrap();
        let encoder = xz2::write::XzEncoder::new(file, 1);
        let mut archive = tar::Builder::new(encoder);
        archive.append_dir(root_name, root).unwrap();
        let mut paths = Vec::new();
        collect_paths(root, root, &mut paths);
        paths.sort();
        for relative in paths {
            let source = root.join(&relative);
            let archive_name = Path::new(root_name).join(&relative);
            if source.is_dir() {
                archive.append_dir(archive_name, source).unwrap();
            } else {
                archive.append_path_with_name(source, archive_name).unwrap();
            }
        }
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();
    }

    fn collect_paths(root: &Path, directory: &Path, paths: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            paths.push(path.strip_prefix(root).unwrap().to_path_buf());
            if path.is_dir() {
                collect_paths(root, &path, paths);
            }
        }
    }

    fn sha256_path(path: &Path) -> String {
        format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
    }
}
