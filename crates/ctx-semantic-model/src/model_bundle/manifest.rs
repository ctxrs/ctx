use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::super::model_contract::{CoreMlBundleContract, COREML_BUNDLE_CONTRACT};

pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_BUNDLE_FILES: usize = 4096;
pub(crate) const MAX_BUNDLE_DIRECTORIES: usize = 1024;
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_BUNDLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const MAX_TOKENIZER_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_METADATA_FILE_BYTES: u64 = 4 * 1024 * 1024;

pub(super) const MAX_PATH_BYTES: usize = 512;
pub(super) const MAX_PATH_COMPONENTS: usize = 64;
pub(super) const MAX_STRING_BYTES: usize = 512;

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

pub(super) fn validate_manifest(manifest: &ModelBundleManifest) -> Result<()> {
    let expected = &COREML_BUNDLE_CONTRACT;
    if manifest.schema_version != expected.schema_version {
        bail!("unsupported model bundle manifest schema version");
    }
    if manifest.bundle_id != expected.bundle_id {
        bail!("model bundle has unsupported bundle id");
    }
    validate_semver(&manifest.bundle_version)?;
    if manifest.model.id != expected.model_id {
        bail!("model bundle has unsupported model id");
    }
    if manifest.model.precision != expected.precision {
        bail!("model bundle must use fp16 precision");
    }
    validate_revision(&manifest.model.source_revision)?;
    if manifest.model.source_revision != expected.source_revision {
        bail!("model bundle has unsupported source revision");
    }
    if manifest.model.embedding_space_id != expected.embedding_space_id {
        bail!("model bundle has unsupported embedding space id");
    }
    if manifest.artifacts.tokenizer != expected.tokenizer_artifact {
        bail!("tokenizer artifact must be tokenizer.json");
    }
    if manifest.artifacts.document_model != expected.document_model_artifact {
        bail!("document model artifact must be document.mlpackage");
    }
    if manifest
        .artifacts
        .query_model
        .as_deref()
        .is_some_and(|path| path != expected.query_model_artifact)
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

    require_manifest_file(&paths, expected.tokenizer_artifact)?;
    require_manifest_prefix(&paths, &format!("{}/", expected.document_model_artifact))?;
    if manifest.artifacts.query_model.is_some() {
        require_manifest_prefix(&paths, &format!("{}/", expected.query_model_artifact))?;
    } else if paths
        .iter()
        .any(|path| path.starts_with(&format!("{}/", expected.query_model_artifact)))
    {
        bail!("query model files are present without a query model artifact");
    }
    require_manifest_file(&paths, "PROVENANCE.json")?;
    require_manifest_file(&paths, "THIRD_PARTY_NOTICES.md")?;
    require_manifest_prefix(&paths, "LICENSES/")?;
    Ok(())
}

pub(super) fn validate_tensor_contract(
    contract: &TensorContract,
    has_query_model: bool,
) -> Result<()> {
    let expected = &COREML_BUNDLE_CONTRACT;
    if contract.inputs.len() != expected.inputs.len() {
        bail!("tensor contract must contain exactly three inputs");
    }
    if contract.document_batch_size != expected.document_batch_size
        || contract.max_sequence_length != expected.max_sequence_length
    {
        bail!("document tensor contract must use fixed batch 16 and sequence length 512");
    }
    match (has_query_model, contract.query_batch_size) {
        (true, query_batch_size) if query_batch_size == expected.query_batch_size => {}
        (false, None) => {}
        (true, _) => bail!("query tensor contract must use fixed batch 1 when present"),
        (false, Some(_)) => {
            bail!("query batch size requires a query model artifact")
        }
    }
    if contract.embedding_dimensions != expected.embedding_dimensions {
        bail!("tensor contract embedding dimension must be 384");
    }
    for (input, (expected_name, expected_dtype)) in
        contract.inputs.iter().zip(expected.inputs.iter().copied())
    {
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
        expected.output_name,
        expected.output_dtype,
        contract.document_batch_size,
        contract.embedding_dimensions,
    )?;
    if contract.document_prefix != expected.document_prefix
        || contract.query_prefix != expected.query_prefix
    {
        bail!("tensor contract has incompatible E5 role prefixes");
    }
    if contract.pooling != expected.pooling || contract.normalization != expected.normalization {
        bail!("tensor contract has incompatible pooling or normalization");
    }
    Ok(())
}

pub(crate) fn validate_bundle_contract(
    bundle: &VerifiedModelBundle,
    expected: &CoreMlBundleContract<'_>,
) -> Result<()> {
    let manifest = &bundle.manifest;
    if bundle.manifest_sha256 != expected.manifest_sha256
        || manifest.schema_version != expected.schema_version
        || manifest.bundle_id != expected.bundle_id
        || manifest.bundle_version != expected.bundle_version
        || manifest.model.id != expected.model_id
        || manifest.model.source_revision != expected.source_revision
        || manifest.model.embedding_space_id != expected.embedding_space_id
        || manifest.model.precision != expected.precision
        || manifest.tensor_contract.document_batch_size != expected.document_batch_size
        || manifest.tensor_contract.query_batch_size != expected.query_batch_size
        || manifest.tensor_contract.max_sequence_length != expected.max_sequence_length
        || manifest.tensor_contract.embedding_dimensions != expected.embedding_dimensions
        || manifest.tensor_contract.document_prefix != expected.document_prefix
        || manifest.tensor_contract.query_prefix != expected.query_prefix
        || manifest.tensor_contract.pooling != expected.pooling
        || manifest.tensor_contract.normalization != expected.normalization
        || manifest.artifacts.tokenizer != expected.tokenizer_artifact
        || manifest.artifacts.document_model != expected.document_model_artifact
        || manifest.artifacts.query_model.as_deref()
            != expected
                .query_batch_size
                .map(|_| expected.query_model_artifact)
    {
        bail!("bundle manifest does not match the compiled descriptor");
    }
    Ok(())
}

pub(super) fn validate_tensor_spec(
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

pub(super) fn validate_semver(value: &str) -> Result<()> {
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

pub(super) fn validate_revision(value: &str) -> Result<()> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("source_revision must be a 40- or 64-character hexadecimal revision");
    }
    Ok(())
}

pub(super) fn deserialize_optional_non_null<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

pub(super) fn validate_short_string(value: &str, name: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_STRING_BYTES || value.chars().any(char::is_control) {
        bail!("{name} is empty, too long, or contains control characters");
    }
    Ok(())
}

pub(super) fn allowed_payload_path(path: &str, query_model: bool) -> bool {
    let expected = &COREML_BUNDLE_CONTRACT;
    path == expected.tokenizer_artifact
        || path == "PROVENANCE.json"
        || path == "THIRD_PARTY_NOTICES.md"
        || path.starts_with("LICENSES/")
        || path.starts_with(&format!("{}/", expected.document_model_artifact))
        || (query_model && path.starts_with(&format!("{}/", expected.query_model_artifact)))
}

pub(super) fn payload_size_limit(path: &str) -> u64 {
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

pub(super) fn require_manifest_file(paths: &BTreeSet<&str>, path: &str) -> Result<()> {
    if !paths.contains(path) {
        bail!("model bundle manifest is missing required file {path}");
    }
    Ok(())
}

pub(super) fn require_manifest_prefix(paths: &BTreeSet<&str>, prefix: &str) -> Result<()> {
    if !paths.iter().any(|path| path.starts_with(prefix)) {
        bail!("model bundle manifest is missing required path {prefix}");
    }
    Ok(())
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

pub(super) fn validate_sha256(value: &str, name: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{name} must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
