use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_BUNDLE_FILES: usize = 4096;
pub(crate) const MAX_BUNDLE_DIRECTORIES: usize = 1024;
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_BUNDLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOKENIZER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_METADATA_FILE_BYTES: u64 = 4 * 1024 * 1024;

const MAX_PATH_BYTES: usize = 512;
const MAX_PATH_COMPONENTS: usize = 64;
const MAX_STRING_BYTES: usize = 512;
const EXPECTED_BUNDLE_ID: &str = "ctx.multilingual-e5-small.coreml.fp16";
const EXPECTED_MODEL_ID: &str = "intfloat/multilingual-e5-small";
const EXPECTED_SOURCE_REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
const EXPECTED_PRECISION: &str = "fp16";
const EXPECTED_EMBEDDING_SPACE_ID: &str = "e5-small-v1:mean-pool:l2:query-passage";
const EXPECTED_INPUTS: [(&str, &str); 3] = [
    ("input_ids", "int32"),
    ("attention_mask", "int32"),
    ("token_type_ids", "int32"),
];
const EXPECTED_OUTPUT_NAME: &str = "sentence_embeddings";
const EXPECTED_OUTPUT_DTYPE: &str = "float32";
const EXPECTED_DIMENSIONS: u32 = 384;
const EXPECTED_DOCUMENT_BATCH_SIZE: u32 = 16;
const EXPECTED_QUERY_BATCH_SIZE: u32 = 1;
const EXPECTED_SEQUENCE_LENGTH: u32 = 512;
const CACHE_NAMESPACE: &str = "semantic-model-bundles";
const COMPLETION_SUFFIX: &str = ".complete.json";

static STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelBundleManifest {
    pub schema_version: u32,
    pub bundle_id: String,
    pub bundle_version: String,
    pub model: ModelIdentity,
    pub tensor_contract: TensorContract,
    pub artifacts: BundleArtifacts,
    pub files: Vec<BundleFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelIdentity {
    pub id: String,
    pub source_revision: String,
    pub embedding_space_id: String,
    pub precision: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TensorContract {
    pub inputs: Vec<TensorSpec>,
    pub output: TensorSpec,
    pub document_batch_size: u32,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub query_batch_size: Option<u32>,
    pub max_sequence_length: u32,
    pub embedding_dimensions: u32,
    pub document_prefix: String,
    pub query_prefix: String,
    pub pooling: String,
    pub normalization: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TensorSpec {
    pub name: String,
    pub dtype: String,
    pub shape: Vec<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundleArtifacts {
    pub tokenizer: String,
    pub document_model: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub query_model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundleFile {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedModelBundle {
    pub root: PathBuf,
    pub manifest: ModelBundleManifest,
    pub manifest_sha256: String,
}

impl VerifiedModelBundle {
    pub(crate) fn tokenizer_path(&self) -> PathBuf {
        self.root.join(&self.manifest.artifacts.tokenizer)
    }

    pub(crate) fn document_model_path(&self) -> PathBuf {
        self.root.join(&self.manifest.artifacts.document_model)
    }

    pub(crate) fn query_model_path(&self) -> Option<PathBuf> {
        self.manifest
            .artifacts
            .query_model
            .as_ref()
            .map(|path| self.root.join(path))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionMarker {
    schema_version: u32,
    manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompileDestination {
    pub final_path: PathBuf,
    pub staging_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtomicCommit {
    Installed,
    AlreadyPresent,
}

pub(crate) fn verify_model_bundle(root: &Path) -> Result<VerifiedModelBundle> {
    require_real_directory(root, "model bundle root")?;
    let manifest_path = root.join(MANIFEST_FILE);
    let manifest_bytes = read_bounded_regular_file(&manifest_path, MAX_MANIFEST_BYTES)
        .with_context(|| format!("read model bundle manifest {}", manifest_path.display()))?;
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let manifest: ModelBundleManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parse model bundle manifest {}", manifest_path.display()))?;
    validate_manifest(&manifest)?;

    let actual_files = collect_bundle_files(root)?;
    let expected_files: BTreeSet<String> = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect();
    if actual_files != expected_files {
        let missing: Vec<_> = expected_files.difference(&actual_files).cloned().collect();
        let unexpected: Vec<_> = actual_files.difference(&expected_files).cloned().collect();
        bail!(
            "model bundle file set does not match manifest (missing: {missing:?}, unexpected: {unexpected:?})"
        );
    }

    for entry in &manifest.files {
        let path = checked_join(root, &entry.path)?;
        verify_file(&path, entry)?;
    }

    Ok(VerifiedModelBundle {
        root: root.to_path_buf(),
        manifest,
        manifest_sha256,
    })
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
    Ok(marker.schema_version == MANIFEST_SCHEMA_VERSION
        && marker.manifest_sha256 == manifest_sha256)
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
        schema_version: MANIFEST_SCHEMA_VERSION,
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

pub(crate) fn prepare_compile_destination(
    compiled_cache_root: &Path,
    manifest_sha256: &str,
    model_role: &str,
    compiler_identity: &str,
) -> Result<CompileDestination> {
    validate_sha256(manifest_sha256, "manifest_sha256")?;
    let role = match model_role {
        "document" | "query" => model_role,
        _ => bail!("compile model role must be document or query"),
    };
    validate_bounded_identifier(compiler_identity, "compiler_identity")?;
    let compiler_hash = sha256_bytes(compiler_identity.as_bytes());
    let parent = compiled_cache_root
        .join("coreml-compiled")
        .join("sha256")
        .join(manifest_sha256)
        .join(compiler_hash);
    create_directory_tree_nofollow(compiled_cache_root, &parent)?;
    let final_path = parent.join(format!("{role}.mlmodelc"));
    reject_symlink_if_present(&final_path)?;
    let staging_path = unique_sibling(&final_path, "compile")?;
    fs::create_dir(&staging_path).with_context(|| {
        format!(
            "create compile staging directory {}",
            staging_path.display()
        )
    })?;
    sync_parent(&staging_path)?;
    Ok(CompileDestination {
        final_path,
        staging_path,
    })
}

pub(crate) fn commit_compile_destination(destination: &CompileDestination) -> Result<AtomicCommit> {
    validate_staging_pair(destination)?;
    require_real_directory(
        &destination.staging_path,
        "compiled model staging directory",
    )?;
    reject_symlinks_recursive(&destination.staging_path)?;
    sync_tree(&destination.staging_path)?;

    match fs::rename(&destination.staging_path, &destination.final_path) {
        Ok(()) => {
            sync_parent(&destination.final_path)?;
            Ok(AtomicCommit::Installed)
        }
        Err(rename_error) => {
            reject_symlink_if_present(&destination.final_path)?;
            if destination.final_path.is_dir() {
                fs::remove_dir_all(&destination.staging_path).with_context(|| {
                    format!(
                        "remove redundant compile staging directory {}",
                        destination.staging_path.display()
                    )
                })?;
                Ok(AtomicCommit::AlreadyPresent)
            } else {
                Err(rename_error).with_context(|| {
                    format!(
                        "atomically publish compiled model {}",
                        destination.final_path.display()
                    )
                })
            }
        }
    }
}

pub(crate) fn discard_compile_destination(destination: &CompileDestination) -> Result<()> {
    validate_staging_pair(destination)?;
    match fs::symlink_metadata(&destination.staging_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to remove symlinked compile staging path")
        }
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&destination.staging_path)
            .with_context(|| {
                format!(
                    "remove compile staging directory {}",
                    destination.staging_path.display()
                )
            }),
        Ok(_) => bail!("compile staging path is not a directory"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect compile staging path {}",
                destination.staging_path.display()
            )
        }),
    }
}

pub(crate) fn invalidate_compiled_model_cache(path: &Path) -> Result<()> {
    let file_name = path.file_name().and_then(|value| value.to_str());
    if !matches!(file_name, Some("document.mlmodelc" | "query.mlmodelc")) {
        bail!("refusing to invalidate unexpected compiled model cache path");
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect compiled model cache {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("refusing to invalidate non-directory compiled model cache");
    }
    fs::remove_dir_all(path)
        .with_context(|| format!("invalidate compiled model cache {}", path.display()))
}

fn validate_manifest(manifest: &ModelBundleManifest) -> Result<()> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        bail!("unsupported model bundle manifest schema version");
    }
    if manifest.bundle_id != EXPECTED_BUNDLE_ID {
        bail!("model bundle has unsupported bundle id");
    }
    validate_semver(&manifest.bundle_version)?;
    if manifest.model.id != EXPECTED_MODEL_ID {
        bail!("model bundle has unsupported model id");
    }
    if manifest.model.precision != EXPECTED_PRECISION {
        bail!("model bundle must use fp16 precision");
    }
    validate_revision(&manifest.model.source_revision)?;
    if manifest.model.source_revision != EXPECTED_SOURCE_REVISION {
        bail!("model bundle has unsupported source revision");
    }
    if manifest.model.embedding_space_id != EXPECTED_EMBEDDING_SPACE_ID {
        bail!("model bundle has unsupported embedding space id");
    }
    if manifest.artifacts.tokenizer != "tokenizer.json" {
        bail!("tokenizer artifact must be tokenizer.json");
    }
    if manifest.artifacts.document_model != "document.mlpackage" {
        bail!("document model artifact must be document.mlpackage");
    }
    if manifest
        .artifacts
        .query_model
        .as_deref()
        .is_some_and(|path| path != "query.mlpackage")
    {
        bail!("query model artifact must be query.mlpackage when present");
    }
    validate_tensor_contract(
        &manifest.tensor_contract,
        manifest.artifacts.query_model.is_some(),
    )?;
    if manifest.files.is_empty() || manifest.files.len() > MAX_BUNDLE_FILES {
        bail!("model bundle file count is outside the allowed range");
    }

    let mut paths = BTreeSet::new();
    let mut total_bytes = 0_u64;
    let mut previous_path: Option<&str> = None;
    for file in &manifest.files {
        validate_relative_path(&file.path)?;
        if !allowed_payload_path(&file.path, manifest.artifacts.query_model.is_some()) {
            bail!(
                "model bundle contains unsupported payload path {}",
                file.path
            );
        }
        if !paths.insert(file.path.as_str()) {
            bail!("duplicate model bundle file path {}", file.path);
        }
        if previous_path.is_some_and(|previous| previous >= file.path.as_str()) {
            bail!("model bundle file records must be sorted by path");
        }
        previous_path = Some(&file.path);
        if file.size_bytes > MAX_FILE_BYTES {
            bail!("model bundle file {} exceeds size limit", file.path);
        }
        if file.size_bytes > payload_size_limit(&file.path) {
            bail!(
                "model bundle file {} exceeds role-specific size limit",
                file.path
            );
        }
        total_bytes = total_bytes
            .checked_add(file.size_bytes)
            .ok_or_else(|| anyhow!("model bundle total size overflow"))?;
        if total_bytes > MAX_BUNDLE_BYTES {
            bail!("model bundle exceeds total size limit");
        }
        validate_sha256(&file.sha256, "file sha256")?;
    }

    require_manifest_file(&paths, "tokenizer.json")?;
    require_manifest_prefix(&paths, "document.mlpackage/")?;
    if manifest.artifacts.query_model.is_some() {
        require_manifest_prefix(&paths, "query.mlpackage/")?;
    } else if paths
        .iter()
        .any(|path| path.starts_with("query.mlpackage/"))
    {
        bail!("query model files are present without a query model artifact");
    }
    require_manifest_file(&paths, "PROVENANCE.json")?;
    require_manifest_file(&paths, "THIRD_PARTY_NOTICES.md")?;
    require_manifest_prefix(&paths, "LICENSES/")?;
    Ok(())
}

fn validate_tensor_contract(contract: &TensorContract, has_query_model: bool) -> Result<()> {
    if contract.inputs.len() != EXPECTED_INPUTS.len() {
        bail!("tensor contract must contain exactly three inputs");
    }
    if contract.document_batch_size != EXPECTED_DOCUMENT_BATCH_SIZE
        || contract.max_sequence_length != EXPECTED_SEQUENCE_LENGTH
    {
        bail!("document tensor contract must use fixed batch 16 and sequence length 512");
    }
    match (has_query_model, contract.query_batch_size) {
        (true, Some(EXPECTED_QUERY_BATCH_SIZE)) | (false, None) => {}
        (true, _) => bail!("query tensor contract must use fixed batch 1 when present"),
        (false, Some(_)) => {
            bail!("query batch size requires a query model artifact")
        }
    }
    if contract.embedding_dimensions != EXPECTED_DIMENSIONS {
        bail!("tensor contract embedding dimension must be 384");
    }
    for (input, (expected_name, expected_dtype)) in contract.inputs.iter().zip(EXPECTED_INPUTS) {
        validate_tensor_spec(
            input,
            expected_name,
            expected_dtype,
            contract.document_batch_size,
            contract.max_sequence_length,
        )?;
    }
    validate_tensor_spec(
        &contract.output,
        EXPECTED_OUTPUT_NAME,
        EXPECTED_OUTPUT_DTYPE,
        contract.document_batch_size,
        contract.embedding_dimensions,
    )?;
    if contract.document_prefix != "passage: " || contract.query_prefix != "query: " {
        bail!("tensor contract has incompatible E5 role prefixes");
    }
    if contract.pooling != "attention_mask_mean" || contract.normalization != "l2" {
        bail!("tensor contract has incompatible pooling or normalization");
    }
    Ok(())
}

fn validate_tensor_spec(
    spec: &TensorSpec,
    expected_name: &str,
    expected_dtype: &str,
    expected_batch: u32,
    expected_width: u32,
) -> Result<()> {
    if spec.name != expected_name || spec.dtype != expected_dtype {
        bail!("tensor contract contains an incompatible tensor");
    }
    let expected_shape = [expected_batch, expected_width];
    if spec.shape.as_slice() != expected_shape {
        bail!("tensor {} has an incompatible shape", spec.name);
    }
    Ok(())
}

fn validate_semver(value: &str) -> Result<()> {
    validate_short_string(value, "bundle_version")?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        || value.matches('+').count() > 1
    {
        bail!("bundle_version must use semantic version syntax");
    }
    let (without_build, build) = value
        .split_once('+')
        .map_or((value, None), |(core, suffix)| (core, Some(suffix)));
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, suffix)| (core, Some(suffix)));
    let parts: Vec<_> = core.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
        || [prerelease, build]
            .into_iter()
            .flatten()
            .any(|suffix| suffix.split('.').any(str::is_empty))
    {
        bail!("bundle_version must use semantic version syntax");
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("source_revision must be a 40- or 64-character hexadecimal revision");
    }
    Ok(())
}

fn deserialize_optional_non_null<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn validate_bounded_identifier(value: &str, name: &str) -> Result<()> {
    validate_short_string(value, name)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/' | b':' | b'_' | b'-')
    }) {
        bail!("{name} contains unsupported characters");
    }
    Ok(())
}

fn validate_short_string(value: &str, name: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_STRING_BYTES || value.chars().any(char::is_control) {
        bail!("{name} is empty, too long, or contains control characters");
    }
    Ok(())
}

fn allowed_payload_path(path: &str, query_model: bool) -> bool {
    path == "tokenizer.json"
        || path == "PROVENANCE.json"
        || path == "THIRD_PARTY_NOTICES.md"
        || path.starts_with("LICENSES/")
        || path.starts_with("document.mlpackage/")
        || (query_model && path.starts_with("query.mlpackage/"))
}

fn payload_size_limit(path: &str) -> u64 {
    if path == "tokenizer.json" {
        MAX_TOKENIZER_BYTES
    } else if path == "PROVENANCE.json"
        || path == "THIRD_PARTY_NOTICES.md"
        || path.starts_with("LICENSES/")
    {
        MAX_METADATA_FILE_BYTES
    } else {
        MAX_FILE_BYTES
    }
}

fn require_manifest_file(paths: &BTreeSet<&str>, path: &str) -> Result<()> {
    if !paths.contains(path) {
        bail!("model bundle manifest is missing required file {path}");
    }
    Ok(())
}

fn require_manifest_prefix(paths: &BTreeSet<&str>, prefix: &str) -> Result<()> {
    if !paths.iter().any(|path| path.starts_with(prefix)) {
        bail!("model bundle manifest is missing required path {prefix}");
    }
    Ok(())
}

fn collect_bundle_files(root: &Path) -> Result<BTreeSet<String>> {
    fn visit(
        root: &Path,
        directory: &Path,
        files: &mut BTreeSet<String>,
        directory_count: &mut usize,
    ) -> Result<()> {
        *directory_count += 1;
        if *directory_count > MAX_BUNDLE_DIRECTORIES {
            bail!("model bundle contains too many directories");
        }
        let mut entries = fs::read_dir(directory)
            .with_context(|| format!("read model bundle directory {}", directory.display()))?
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        if entries.is_empty() && directory != root {
            bail!(
                "model bundle contains empty directory {}",
                directory.display()
            );
        }
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("inspect model bundle path {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!("model bundle contains symlink {}", path.display());
            }
            if metadata.is_dir() {
                visit(root, &path, files, directory_count)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| anyhow!("model bundle path escaped root"))?
                    .to_str()
                    .ok_or_else(|| anyhow!("model bundle path is not UTF-8"))?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                validate_relative_path(&relative)?;
                if relative != MANIFEST_FILE {
                    if files.len() >= MAX_BUNDLE_FILES {
                        bail!("model bundle contains too many files");
                    }
                    files.insert(relative);
                }
            } else {
                bail!("model bundle contains unsupported path {}", path.display());
            }
        }
        Ok(())
    }

    let mut files = BTreeSet::new();
    let mut directory_count = 0;
    visit(root, root, &mut files, &mut directory_count)?;
    Ok(files)
}

fn verify_file(path: &Path, entry: &BundleFile) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect model bundle file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "model bundle path is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() != entry.size_bytes {
        bail!("model bundle file size mismatch for {}", entry.path);
    }
    let mut file = open_read_nofollow(path)?;
    let opened_metadata = file
        .metadata()
        .with_context(|| format!("inspect opened model bundle file {}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() != entry.size_bytes {
        bail!("model bundle file changed while opening: {}", entry.path);
    }
    let actual = sha256_reader(&mut file, entry.size_bytes, path)?;
    if actual != entry.sha256 {
        bail!("model bundle SHA-256 mismatch for {}", entry.path);
    }
    let after = fs::symlink_metadata(path)
        .with_context(|| format!("reinspect model bundle file {}", path.display()))?;
    if after.file_type().is_symlink() || !same_file_metadata(&opened_metadata, &after) {
        bail!(
            "model bundle file changed during verification: {}",
            entry.path
        );
    }
    Ok(())
}

fn sha256_reader(file: &mut File, expected_size: u64, path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut read_bytes = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("hash model bundle file {}", path.display()))?;
        if count == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("model bundle file size overflow"))?;
        if read_bytes > expected_size || read_bytes > MAX_FILE_BYTES {
            bail!("model bundle file grew while hashing: {}", path.display());
        }
        hasher.update(&buffer[..count]);
    }
    if read_bytes != expected_size {
        bail!(
            "model bundle file changed size while hashing: {}",
            path.display()
        );
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn read_bounded_regular_file(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("path is not a regular non-symlink file: {}", path.display());
    }
    if metadata.len() > maximum {
        bail!("file exceeds size limit: {}", path.display());
    }
    let mut file = open_read_nofollow(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        bail!("file exceeds size limit: {}", path.display());
    }
    Ok(bytes)
}

fn checked_join(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_path(relative)?;
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            bail!("invalid model bundle path {relative}");
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect model bundle path {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("model bundle path traverses symlink {}", path.display());
        }
    }
    Ok(path)
}

pub(crate) fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.contains('\\')
        || path.contains(':')
        || path.starts_with('/')
        || path.ends_with('/')
    {
        bail!("invalid model bundle relative path {path:?}");
    }
    let components: Vec<_> = Path::new(path).components().collect();
    if components.is_empty()
        || components.len() > MAX_PATH_COMPONENTS
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("invalid model bundle relative path {path:?}");
    }
    if path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("invalid model bundle relative path {path:?}");
    }
    Ok(())
}

fn validate_sha256(value: &str, name: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{name} must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn require_real_directory(path: &Path, description: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {description} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{description} must be a real directory: {}", path.display());
    }
    Ok(())
}

fn create_directory_tree_nofollow(base: &Path, leaf: &Path) -> Result<()> {
    let relative = leaf
        .strip_prefix(base)
        .map_err(|_| anyhow!("cache path escaped its root"))?;
    require_real_directory(base, "compiled cache root")?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            bail!("compiled cache path contains invalid component");
        };
        current.push(component);
        match fs::create_dir(&current) {
            Ok(()) => sync_parent(&current)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create compiled cache directory {}", current.display())
                });
            }
        }
        require_real_directory(&current, "compiled cache directory")?;
    }
    Ok(())
}

fn reject_symlink_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing symlink path {}", path.display())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspect path {}", path.display())),
    }
}

fn reject_symlinks_recursive(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!(
                "compiled model staging tree contains symlink {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            reject_symlinks_recursive(&path)?;
        } else if !metadata.is_file() {
            bail!(
                "compiled model staging tree contains unsupported path {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn sync_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            sync_tree(&path)?;
        } else if metadata.is_file() {
            File::open(&path)?.sync_all()?;
        }
    }
    sync_directory(root)
}

fn unique_sibling(destination: &Path, purpose: &str) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent"))?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("destination has no UTF-8 file name"))?;
    for _ in 0..128 {
        let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.{purpose}.{}.{}.tmp", process::id(), nonce));
        if !candidate.exists() {
            reject_symlink_if_present(&candidate)?;
            return Ok(candidate);
        }
    }
    bail!("could not allocate unique staging path")
}

fn validate_staging_pair(destination: &CompileDestination) -> Result<()> {
    if destination.final_path.parent() != destination.staging_path.parent() {
        bail!("compile staging and final paths must share a parent");
    }
    let final_name = destination
        .final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("compile final path has no UTF-8 file name"))?;
    let staging_name = destination
        .staging_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("compile staging path has no UTF-8 file name"))?;
    if !staging_name.starts_with(&format!(".{final_name}.compile."))
        || !staging_name.ends_with(".tmp")
    {
        bail!("compile staging path is not associated with final path");
    }
    Ok(())
}

#[cfg(unix)]
fn open_read_nofollow(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| {
            format!(
                "open regular file without following symlinks {}",
                path.display()
            )
        })
}

#[cfg(windows)]
fn open_read_nofollow(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| {
            format!(
                "open regular file without following reparse points {}",
                path.display()
            )
        })
}

#[cfg(not(any(unix, windows)))]
fn open_read_nofollow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open regular file {}", path.display()))
}

#[cfg(unix)]
fn create_new_nofollow(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("create file without following symlinks {}", path.display()))
}

#[cfg(not(unix))]
fn create_new_nofollow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create file {}", path.display()))
}

#[cfg(unix)]
fn same_file_metadata(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
}

#[cfg(not(unix))]
fn same_file_metadata(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open directory for sync {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    sync_directory(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn valid_manifest(root: &Path) -> ModelBundleManifest {
        let payloads = [
            ("tokenizer.json", b"{}".as_slice()),
            ("document.mlpackage/Data/model.bin", b"model".as_slice()),
            ("PROVENANCE.json", b"{}".as_slice()),
            ("THIRD_PARTY_NOTICES.md", b"notices\n".as_slice()),
            ("LICENSES/MODEL_LICENSE.txt", b"license\n".as_slice()),
        ];
        let mut files: Vec<_> = payloads
            .into_iter()
            .map(|(path, body)| {
                write(&root.join(path), body);
                BundleFile {
                    path: path.to_owned(),
                    size_bytes: body.len() as u64,
                    sha256: sha256_bytes(body),
                }
            })
            .collect();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        ModelBundleManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            bundle_id: "ctx.multilingual-e5-small.coreml.fp16".to_owned(),
            bundle_version: "1.0.0".to_owned(),
            model: ModelIdentity {
                id: EXPECTED_MODEL_ID.to_owned(),
                source_revision: "614241f622f53c4eeff9890bdc4f31cfecc418b3".to_owned(),
                embedding_space_id: "e5-small-v1:mean-pool:l2:query-passage".to_owned(),
                precision: EXPECTED_PRECISION.to_owned(),
            },
            tensor_contract: TensorContract {
                inputs: EXPECTED_INPUTS
                    .into_iter()
                    .map(|(name, dtype)| TensorSpec {
                        name: name.to_owned(),
                        dtype: dtype.to_owned(),
                        shape: vec![16, 512],
                    })
                    .collect(),
                output: TensorSpec {
                    name: EXPECTED_OUTPUT_NAME.to_owned(),
                    dtype: EXPECTED_OUTPUT_DTYPE.to_owned(),
                    shape: vec![16, EXPECTED_DIMENSIONS],
                },
                document_batch_size: 16,
                query_batch_size: None,
                max_sequence_length: 512,
                embedding_dimensions: EXPECTED_DIMENSIONS,
                document_prefix: "passage: ".to_owned(),
                query_prefix: "query: ".to_owned(),
                pooling: "attention_mask_mean".to_owned(),
                normalization: "l2".to_owned(),
            },
            artifacts: BundleArtifacts {
                tokenizer: "tokenizer.json".to_owned(),
                document_model: "document.mlpackage".to_owned(),
                query_model: None,
            },
            files,
        }
    }

    fn create_valid_bundle(root: &Path) -> ModelBundleManifest {
        let manifest = valid_manifest(root);
        write_manifest(root, &manifest);
        manifest
    }

    fn add_query_model(root: &Path, manifest: &mut ModelBundleManifest) {
        let path = "query.mlpackage/Data/model.bin";
        let body = b"query model";
        write(&root.join(path), body);
        manifest.files.push(BundleFile {
            path: path.to_owned(),
            size_bytes: body.len() as u64,
            sha256: sha256_bytes(body),
        });
        manifest
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        manifest.artifacts.query_model = Some("query.mlpackage".to_owned());
        manifest.tensor_contract.query_batch_size = Some(1);
    }

    fn write_manifest(root: &Path, manifest: &ModelBundleManifest) {
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        fs::write(root.join(MANIFEST_FILE), bytes).unwrap();
    }

    #[test]
    fn verifies_complete_bundle_and_reports_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        let verified = verify_model_bundle(temp.path()).unwrap();
        assert_eq!(verified.manifest.model.id, EXPECTED_MODEL_ID);
        assert_eq!(
            verified.tokenizer_path(),
            temp.path().join("tokenizer.json")
        );
        assert_eq!(
            verified.document_model_path(),
            temp.path().join("document.mlpackage")
        );
        assert_eq!(verified.query_model_path(), None);
        validate_sha256(&verified.manifest_sha256, "test hash").unwrap();
    }

    #[test]
    fn verifies_distinct_document_and_query_batch_contracts() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        add_query_model(temp.path(), &mut manifest);
        write_manifest(temp.path(), &manifest);

        let verified = verify_model_bundle(temp.path()).unwrap();
        assert_eq!(verified.manifest.tensor_contract.document_batch_size, 16);
        assert_eq!(verified.manifest.tensor_contract.query_batch_size, Some(1));
        assert_eq!(
            verified.query_model_path(),
            Some(temp.path().join("query.mlpackage"))
        );
    }

    #[test]
    fn rejects_hash_mismatch_and_unlisted_payload() {
        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        fs::write(temp.path().join("tokenizer.json"), b"changed").unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("size mismatch"));

        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        fs::write(temp.path().join("unexpected"), b"x").unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("file set"));
    }

    #[test]
    fn rejects_traversal_unknown_fields_and_oversized_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = create_valid_bundle(temp.path());
        manifest.files[0].path = "../tokenizer.json".to_owned();
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("relative path"));

        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        value["unknown"] = serde_json::json!(true);
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path()).is_err());

        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join(MANIFEST_FILE),
            vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
        )
        .unwrap();
        let error = verify_model_bundle(temp.path()).unwrap_err();
        assert!(format!("{error:#}").contains("size limit"));
    }

    #[test]
    fn rejects_unpinned_model_source_revision() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = create_valid_bundle(temp.path());
        manifest.model.source_revision = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("source revision"));
    }

    #[test]
    fn rejects_nonproduction_document_tensor_shape() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = create_valid_bundle(temp.path());
        manifest.tensor_contract.document_batch_size = 512;
        for input in &mut manifest.tensor_contract.inputs {
            input.shape[0] = 512;
        }
        manifest.tensor_contract.output.shape[0] = 512;
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("document tensor contract must use fixed batch 16"));
    }

    #[test]
    fn rejects_swapped_role_batches_and_query_contract_mismatches() {
        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        add_query_model(temp.path(), &mut manifest);
        manifest.tensor_contract.document_batch_size = 1;
        manifest.tensor_contract.query_batch_size = Some(16);
        for input in &mut manifest.tensor_contract.inputs {
            input.shape[0] = 1;
        }
        manifest.tensor_contract.output.shape[0] = 1;
        write_manifest(temp.path(), &manifest);
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("document tensor contract must use fixed batch 16"));

        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        add_query_model(temp.path(), &mut manifest);
        manifest.tensor_contract.query_batch_size = None;
        write_manifest(temp.path(), &manifest);
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("query tensor contract must use fixed batch 1"));

        let temp = tempfile::tempdir().unwrap();
        let mut manifest = valid_manifest(temp.path());
        manifest.tensor_contract.query_batch_size = Some(1);
        write_manifest(temp.path(), &manifest);
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("query batch size requires a query model artifact"));
    }

    #[test]
    fn rejects_legacy_batch_size_field() {
        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.path().join(MANIFEST_FILE)).unwrap()).unwrap();
        value["tensor_contract"]["batch_size"] = serde_json::json!(16);
        value["tensor_contract"]
            .as_object_mut()
            .unwrap()
            .remove("document_batch_size");
        fs::write(
            temp.path().join(MANIFEST_FILE),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(verify_model_bundle(temp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_in_bundle_and_manifest() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        create_valid_bundle(temp.path());
        fs::remove_file(temp.path().join("tokenizer.json")).unwrap();
        symlink("PROVENANCE.json", temp.path().join("tokenizer.json")).unwrap();
        assert!(verify_model_bundle(temp.path())
            .unwrap_err()
            .to_string()
            .contains("symlink"));

        let outside = tempfile::NamedTempFile::new().unwrap();
        let root = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join(MANIFEST_FILE)).unwrap();
        assert!(verify_model_bundle(root.path()).is_err());
    }

    #[test]
    fn builds_content_addressed_path_and_atomic_marker() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        create_valid_bundle(&source);
        let hash = verify_model_bundle(&source).unwrap().manifest_sha256;
        let bundle = content_addressed_bundle_path(temp.path(), &hash).unwrap();
        fs::create_dir_all(bundle.parent().unwrap()).unwrap();
        fs::rename(&source, &bundle).unwrap();
        assert!(bundle.ends_with(format!("{}/{hash}", &hash[..2])));
        assert!(!completion_marker_matches(&bundle, &hash).unwrap());
        write_completion_marker_atomic(&bundle).unwrap();
        assert!(completion_marker_matches(&bundle, &hash).unwrap());
        assert!(content_addressed_bundle_path(temp.path(), &"A".repeat(64)).is_err());
    }

    #[test]
    fn compile_destination_commits_atomically_and_reuses_winner() {
        let temp = tempfile::tempdir().unwrap();
        let hash = "b".repeat(64);
        let first =
            prepare_compile_destination(temp.path(), &hash, "document", "coremltools-8.3").unwrap();
        write(&first.staging_path.join("model.bin"), b"compiled");
        assert_eq!(
            commit_compile_destination(&first).unwrap(),
            AtomicCommit::Installed
        );
        assert_eq!(
            fs::read(first.final_path.join("model.bin")).unwrap(),
            b"compiled"
        );

        let second =
            prepare_compile_destination(temp.path(), &hash, "document", "coremltools-8.3").unwrap();
        write(&second.staging_path.join("model.bin"), b"loser");
        assert_eq!(
            commit_compile_destination(&second).unwrap(),
            AtomicCommit::AlreadyPresent
        );
        assert!(!second.staging_path.exists());
        assert_eq!(
            fs::read(second.final_path.join("model.bin")).unwrap(),
            b"compiled"
        );
    }

    #[test]
    fn corrupt_compiled_cache_can_be_invalidated_once() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("document.mlmodelc");
        fs::create_dir(&cache).unwrap();
        write(&cache.join("corrupt.bin"), b"corrupt");
        invalidate_compiled_model_cache(&cache).unwrap();
        assert!(!cache.exists());
        assert!(invalidate_compiled_model_cache(&cache).is_err());
        assert!(invalidate_compiled_model_cache(&temp.path().join("unexpected")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn compile_destination_rejects_symlinked_output_tree() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let destination =
            prepare_compile_destination(temp.path(), &"c".repeat(64), "query", "coremltools-8.3")
                .unwrap();
        symlink("missing", destination.staging_path.join("link")).unwrap();
        assert!(commit_compile_destination(&destination).is_err());
        discard_compile_destination(&destination).unwrap();
    }
}
