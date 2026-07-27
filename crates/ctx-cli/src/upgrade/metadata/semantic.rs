use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::semantic::{
    semantic_provisioning_coreml_asset_matches, semantic_provisioning_model_contract_matches,
    semantic_provisioning_model_path_count, semantic_provisioning_model_path_matches,
    semantic_required_model_file_count, semantic_required_model_file_matches,
    SemanticOrtModelVariant,
};

use super::validate_artifact_name;

#[path = "semantic_validation.rs"]
mod validation;
use validation::{
    validate_archive_prefix, validate_asset_id, validate_lowercase_sha256, validate_relative_path,
};

const SEMANTIC_METADATA_PREFIX: &str = "CTX_RELEASE_SEMANTIC_";
const SEMANTIC_SCHEMA_KEY: &str = "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION";
const SEMANTIC_ASSETS_KEY: &str = "CTX_RELEASE_SEMANTIC_ASSETS";
const AUTHORITY_KEYS: [(&str, &str, &str); 4] = [
    ("apple_silicon_coreml", "apple-silicon", "coreml"),
    ("windows_windows_ml", "windows", "windows-ml"),
    ("linux_nvidia_ort_cuda", "linux-nvidia", "ort-cuda"),
    ("universal_ort_cpu", "universal", "ort-cpu"),
];
const CPU_MODEL_ASSET_ID: &str = "onnx_model";
const ACCELERATOR_MODEL_ASSET_ID: &str = "onnx_model_o4_fp16";
const CPU_MODEL_ARTIFACT: &str = "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz";
const ACCELERATOR_MODEL_ARTIFACT: &str = "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz";
const WINDOWS_ML_ARTIFACT: &str = "ctx-windowsml-windows-x64.zip";
const WINDOWS_ML_FILES: [&str; 5] = [
    "LICENSE",
    "ThirdPartyNotices.txt",
    "lib/DirectML.dll",
    "lib/Microsoft.Windows.AI.MachineLearning.dll",
    "lib/onnxruntime.dll",
];
const CPU_PLATFORMS: [&str; 6] = [
    "linux-x64",
    "linux-aarch64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
    "freebsd-x64",
];

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::upgrade) struct SemanticFileMetadata {
    pub(in crate::upgrade) path: String,
    pub(in crate::upgrade) size: u64,
    pub(in crate::upgrade) sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::upgrade) struct SemanticAssetMetadata {
    pub(in crate::upgrade) role: String,
    pub(in crate::upgrade) backend: String,
    pub(in crate::upgrade) version: String,
    pub(in crate::upgrade) platform: String,
    pub(in crate::upgrade) artifact: String,
    pub(in crate::upgrade) archive_format: String,
    pub(in crate::upgrade) archive_path_prefix: String,
    pub(in crate::upgrade) archive_sha256: String,
    pub(in crate::upgrade) max_expanded_bytes: u64,
    pub(in crate::upgrade) max_files: u32,
    pub(in crate::upgrade) files: Vec<SemanticFileMetadata>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticAssetCatalog {
    schema_version: u32,
    assets: BTreeMap<String, SemanticAssetMetadata>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticModelContract {
    model_id: String,
    revision: String,
    dimensions: u32,
    pooling: String,
    normalization: String,
    query_prefix: String,
    passage_prefix: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SemanticAuthorityMetadata {
    schema_version: u32,
    target: String,
    backend: String,
    model_contract: SemanticModelContract,
    runtime_install_manifest_schema_version: u32,
    asset_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(in crate::upgrade) struct SemanticReleaseMetadata {
    assets: BTreeMap<String, SemanticAssetMetadata>,
    authorities: BTreeMap<String, SemanticAuthorityMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::upgrade) enum SemanticAccelerator {
    CoreMl,
    WindowsMl,
    OrtCuda,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) struct SelectedSemanticAsset {
    pub(in crate::upgrade) id: String,
    pub(in crate::upgrade) metadata: SemanticAssetMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) struct SelectedSemanticProvisioning {
    pub(in crate::upgrade) target: &'static str,
    pub(in crate::upgrade) backend: &'static str,
    pub(in crate::upgrade) assets: Vec<SelectedSemanticAsset>,
}

impl SemanticReleaseMetadata {
    pub(in crate::upgrade) fn select(
        &self,
        platform: &str,
        accelerator: Option<SemanticAccelerator>,
    ) -> Result<SelectedSemanticProvisioning> {
        let (authority_key, target, backend) = match accelerator {
            Some(SemanticAccelerator::CoreMl) if platform == "macos-arm64" => {
                ("apple_silicon_coreml", "apple-silicon", "coreml")
            }
            Some(SemanticAccelerator::WindowsMl) if platform == "windows-x64" => {
                ("windows_windows_ml", "windows", "windows-ml")
            }
            Some(SemanticAccelerator::OrtCuda) if platform == "linux-x64" => {
                ("linux_nvidia_ort_cuda", "linux-nvidia", "ort-cuda")
            }
            Some(accelerator) => {
                return Err(anyhow!(
                    "Semantic accelerator {accelerator:?} is incompatible with {platform}"
                ));
            }
            None => ("universal_ort_cpu", "universal", "ort-cpu"),
        };
        let authority = self.authority(authority_key)?;
        let ids = match accelerator {
            None => vec![
                authority_asset_id(self, authority, "model", "any")?,
                authority_asset_id(self, authority, "cpu-runtime", platform)?,
            ],
            Some(_) => authority.asset_ids.clone(),
        };
        let assets = ids
            .into_iter()
            .map(|id| {
                let metadata = self
                    .assets
                    .get(&id)
                    .ok_or_else(|| anyhow!("Semantic authority references unknown asset {id}"))?
                    .clone();
                Ok(SelectedSemanticAsset { id, metadata })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(SelectedSemanticProvisioning {
            target,
            backend,
            assets,
        })
    }

    fn authority(&self, key: &str) -> Result<&SemanticAuthorityMetadata> {
        self.authorities
            .get(key)
            .ok_or_else(|| anyhow!("Semantic metadata is missing authority {key}"))
    }
}

pub(super) fn parse_semantic_metadata(
    metadata: &BTreeMap<String, String>,
) -> Result<Option<SemanticReleaseMetadata>> {
    let semantic_keys = metadata
        .keys()
        .filter(|key| key.starts_with(SEMANTIC_METADATA_PREFIX))
        .cloned()
        .collect::<Vec<_>>();
    if semantic_keys.is_empty() {
        return Ok(None);
    }
    let expected_keys = std::iter::once(SEMANTIC_SCHEMA_KEY.to_owned())
        .chain(std::iter::once(SEMANTIC_ASSETS_KEY.to_owned()))
        .chain(
            AUTHORITY_KEYS
                .iter()
                .map(|(key, _, _)| format!("CTX_RELEASE_SEMANTIC_AUTHORITY_{key}")),
        )
        .collect::<BTreeSet<_>>();
    let actual_keys = semantic_keys.into_iter().collect::<BTreeSet<_>>();
    if actual_keys != expected_keys {
        let missing = expected_keys
            .difference(&actual_keys)
            .cloned()
            .collect::<Vec<_>>();
        let unexpected = actual_keys
            .difference(&expected_keys)
            .cloned()
            .collect::<Vec<_>>();
        return Err(anyhow!(
            "metadata has wrong Semantic field set; missing [{}], unexpected [{}]",
            missing.join(", "),
            unexpected.join(", ")
        ));
    }
    if metadata.get(SEMANTIC_SCHEMA_KEY).map(String::as_str) != Some("1") {
        return Err(anyhow!(
            "unsupported Semantic release metadata schema; expected 1"
        ));
    }

    let catalog: SemanticAssetCatalog = decode_canonical_record(metadata, SEMANTIC_ASSETS_KEY)?;
    if catalog.schema_version != 1 || catalog.assets.is_empty() {
        return Err(anyhow!(
            "Semantic asset catalog has unsupported schema or no assets"
        ));
    }
    for (id, asset) in &catalog.assets {
        validate_asset_id(id)?;
        validate_asset(asset).with_context(|| format!("validate Semantic asset {id}"))?;
    }

    let mut authorities = BTreeMap::new();
    for (key, target, backend) in AUTHORITY_KEYS {
        let field = format!("CTX_RELEASE_SEMANTIC_AUTHORITY_{key}");
        let authority: SemanticAuthorityMetadata = decode_canonical_record(metadata, &field)?;
        validate_authority(&authority, target, backend)
            .with_context(|| format!("validate metadata {field}"))?;
        authorities.insert(key.to_owned(), authority);
    }
    let result = SemanticReleaseMetadata {
        assets: catalog.assets,
        authorities,
    };
    validate_composition(&result)?;
    Ok(Some(result))
}

fn decode_canonical_record<T: DeserializeOwned>(
    metadata: &BTreeMap<String, String>,
    field: &str,
) -> Result<T> {
    let encoded = metadata
        .get(field)
        .ok_or_else(|| anyhow!("metadata missing {field}"))?;
    let decoded = BASE64
        .decode(encoded)
        .with_context(|| format!("metadata {field} is not canonical base64"))?;
    if BASE64.encode(&decoded) != *encoded {
        return Err(anyhow!("metadata {field} is not canonical base64"));
    }
    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .with_context(|| format!("metadata {field} is not JSON"))?;
    if serde_json::to_vec(&value)? != decoded {
        return Err(anyhow!("metadata {field} is not canonical JSON"));
    }
    serde_json::from_value(value)
        .with_context(|| format!("metadata {field} has an invalid signed record shape"))
}

fn validate_authority(
    authority: &SemanticAuthorityMetadata,
    target: &str,
    backend: &str,
) -> Result<()> {
    if authority.schema_version != 1
        || authority.target != target
        || authority.backend != backend
        || authority.runtime_install_manifest_schema_version != 1
    {
        return Err(anyhow!(
            "Semantic authority identity or runtime manifest schema does not match {target}/{backend}"
        ));
    }
    let model = &authority.model_contract;
    if !semantic_provisioning_model_contract_matches(
        &model.model_id,
        &model.revision,
        model.dimensions,
        &model.pooling,
        &model.normalization,
        &model.query_prefix,
        &model.passage_prefix,
    ) {
        return Err(anyhow!(
            "Semantic authority does not use the pinned multilingual E5 contract"
        ));
    }
    if authority.asset_ids.is_empty() {
        return Err(anyhow!("Semantic authority has no assets"));
    }
    let mut ids = BTreeSet::new();
    for id in &authority.asset_ids {
        validate_asset_id(id)?;
        if !ids.insert(id) {
            return Err(anyhow!("Semantic authority repeats asset {id}"));
        }
    }
    Ok(())
}

fn validate_composition(metadata: &SemanticReleaseMetadata) -> Result<()> {
    let universal = metadata.authority("universal_ort_cpu")?;
    let cpu_model_id = authority_asset_id(metadata, universal, "model", "any")?;
    if cpu_model_id != CPU_MODEL_ASSET_ID {
        return Err(anyhow!(
            "Semantic catalog uses the wrong pinned CPU model asset ID"
        ));
    }
    validate_model_asset(
        metadata
            .assets
            .get(&cpu_model_id)
            .expect("validated Semantic asset reference"),
        SemanticOrtModelVariant::CpuFp32,
        CPU_MODEL_ARTIFACT,
    )?;
    if universal.asset_ids.len() != CPU_PLATFORMS.len() + 1
        || universal.asset_ids.first().map(String::as_str) != Some(CPU_MODEL_ASSET_ID)
    {
        return Err(anyhow!(
            "universal/ort-cpu must bind the FP32 model followed by every public CPU runtime"
        ));
    }
    for platform in CPU_PLATFORMS {
        authority_asset_id(metadata, universal, "cpu-runtime", platform)?;
    }

    let windows = metadata.authority("windows_windows_ml")?;
    let accelerator_model_id = authority_asset_id(metadata, windows, "model", "any")?;
    if accelerator_model_id != ACCELERATOR_MODEL_ASSET_ID {
        return Err(anyhow!(
            "Semantic catalog uses the wrong pinned accelerator model asset ID"
        ));
    }
    validate_model_asset(
        metadata
            .assets
            .get(&accelerator_model_id)
            .expect("validated Semantic asset reference"),
        SemanticOrtModelVariant::AcceleratorO4Fp16,
        ACCELERATOR_MODEL_ARTIFACT,
    )?;
    if windows.asset_ids.len() != 2
        || windows.asset_ids[0] != accelerator_model_id
        || windows.asset_ids[1]
            != authority_asset_id(metadata, windows, "cpu-runtime", "windows-x64")?
    {
        return Err(anyhow!(
            "Windows ML authority must bind the O4 FP16 model and one self-contained Windows ML runtime"
        ));
    }

    let cuda = metadata.authority("linux_nvidia_ort_cuda")?;
    if cuda.asset_ids.len() != 2
        || cuda.asset_ids[0] != accelerator_model_id
        || cuda.asset_ids[1]
            != authority_asset_id(metadata, cuda, "accelerator", "linux-x64-cuda12")?
    {
        return Err(anyhow!(
            "CUDA authority must bind the O4 FP16 model and CUDA runtime"
        ));
    }

    let coreml = metadata.authority("apple_silicon_coreml")?;
    if coreml.asset_ids.len() != 3
        || coreml.asset_ids[0] != cpu_model_id
        || coreml.asset_ids[1]
            != authority_asset_id(metadata, coreml, "cpu-runtime", "macos-arm64")?
        || coreml.asset_ids[2]
            != authority_asset_id(metadata, coreml, "accelerator", "macos-arm64")?
    {
        return Err(anyhow!(
            "Core ML authority must bind the FP32 model, macOS arm64 CPU runtime, and Core ML bundle"
        ));
    }

    let referenced = metadata
        .authorities
        .values()
        .flat_map(|authority| authority.asset_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let catalog = metadata.assets.keys().cloned().collect::<BTreeSet<_>>();
    if referenced != catalog {
        return Err(anyhow!(
            "Semantic asset catalog and authority references do not exactly match"
        ));
    }
    Ok(())
}

fn validate_model_asset(
    model: &SemanticAssetMetadata,
    variant: SemanticOrtModelVariant,
    artifact: &str,
) -> Result<()> {
    if model.artifact != artifact
        || model.files.len() != semantic_provisioning_model_path_count()
        || !model
            .files
            .iter()
            .all(|file| semantic_provisioning_model_path_matches(&file.path))
        || model
            .files
            .iter()
            .filter(|file| {
                semantic_required_model_file_matches(variant, &file.path, file.size, &file.sha256)
            })
            .count()
            != semantic_required_model_file_count(variant)
    {
        return Err(anyhow!(
            "Semantic {} model asset does not exactly match the signed provisioning package inventory and compiled runtime model identity",
            variant.as_str()
        ));
    }
    Ok(())
}

fn authority_asset_id(
    metadata: &SemanticReleaseMetadata,
    authority: &SemanticAuthorityMetadata,
    role: &str,
    platform: &str,
) -> Result<String> {
    let matches = authority
        .asset_ids
        .iter()
        .filter(|id| {
            metadata
                .assets
                .get(*id)
                .is_some_and(|asset| asset.role == role && asset.platform == platform)
        })
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(anyhow!(
            "Semantic authority {}/{} has no {role} asset for {platform}",
            authority.target,
            authority.backend
        )),
        _ => Err(anyhow!(
            "Semantic authority {}/{} has duplicate {role} assets for {platform}",
            authority.target,
            authority.backend
        )),
    }
}

fn validate_asset(asset: &SemanticAssetMetadata) -> Result<()> {
    validate_asset_identity(asset)?;
    validate_artifact_name(&asset.artifact)?;
    validate_lowercase_sha256(&asset.archive_sha256)?;
    validate_archive_prefix(&asset.archive_path_prefix)?;
    if asset.max_expanded_bytes == 0
        || asset.max_expanded_bytes > 2 * 1024 * 1024 * 1024
        || asset.max_files == 0
        || asset.max_files > 4096
        || asset.files.is_empty()
        || asset.files.len() > asset.max_files as usize
    {
        return Err(anyhow!(
            "Semantic asset {} has unsafe archive limits",
            asset.artifact
        ));
    }
    let mut total = 0_u64;
    let mut previous = None::<&str>;
    let mut folded = BTreeSet::new();
    for file in &asset.files {
        validate_relative_path(&file.path)?;
        if (asset.backend.starts_with("ort-") || asset.backend == "windows-ml")
            && file.path == "ctx-runtime-install.json"
        {
            return Err(anyhow!(
                "Semantic runtime archive claims the reserved install manifest path"
            ));
        }
        if previous.is_some_and(|value| value >= file.path.as_str()) {
            return Err(anyhow!(
                "Semantic asset {} file records are not strictly sorted",
                asset.artifact
            ));
        }
        previous = Some(&file.path);
        if !folded.insert(file.path.to_ascii_lowercase()) {
            return Err(anyhow!(
                "Semantic asset {} contains duplicate or case-colliding paths",
                asset.artifact
            ));
        }
        if file.size == 0 {
            return Err(anyhow!(
                "Semantic asset {} contains an empty file",
                asset.artifact
            ));
        }
        validate_lowercase_sha256(&file.sha256)?;
        total = total
            .checked_add(file.size)
            .ok_or_else(|| anyhow!("Semantic asset expanded size overflows"))?;
    }
    if total > asset.max_expanded_bytes {
        return Err(anyhow!(
            "Semantic asset {} exceeds its signed expanded-size limit",
            asset.artifact
        ));
    }
    Ok(())
}

fn validate_asset_identity(asset: &SemanticAssetMetadata) -> Result<()> {
    let allowed = match (asset.role.as_str(), asset.backend.as_str()) {
        ("model", "onnx") => {
            asset.version == "1.0.0"
                && asset.platform == "any"
                && matches!(
                    asset.artifact.as_str(),
                    CPU_MODEL_ARTIFACT | ACCELERATOR_MODEL_ARTIFACT
                )
                && asset.archive_format == "tar.xz"
        }
        ("cpu-runtime", "ort-cpu") => {
            asset.version == "1.27.0"
                && CPU_PLATFORMS
                    .iter()
                    .any(|platform| *platform != "windows-x64" && *platform == asset.platform)
                && archive_format_matches_platform(asset)
        }
        ("cpu-runtime", "windows-ml") => {
            asset.version == "2.1.74"
                && asset.platform == "windows-x64"
                && asset.artifact == WINDOWS_ML_ARTIFACT
                && asset.archive_format == "zip"
                && asset.files.len() == WINDOWS_ML_FILES.len()
                && asset
                    .files
                    .iter()
                    .map(|file| file.path.as_str())
                    .eq(WINDOWS_ML_FILES)
        }
        ("accelerator", "coreml") => {
            let manifest_sha256 = asset
                .files
                .iter()
                .find(|file| file.path == "manifest.json")
                .map(|file| file.sha256.as_str())
                .unwrap_or_default();
            asset.version == "1.0.0"
                && asset.platform == "macos-arm64"
                && asset.archive_format == "tar.xz"
                && semantic_provisioning_coreml_asset_matches(
                    &asset.artifact,
                    &asset.archive_sha256,
                    manifest_sha256,
                )
        }
        ("accelerator", "ort-cuda") => {
            asset.version == "1.27.0"
                && asset.platform == "linux-x64-cuda12"
                && asset.archive_format == "tar.zst"
        }
        _ => false,
    };
    if !allowed {
        return Err(anyhow!(
            "unsupported Semantic asset {}/{}/{}/{}",
            asset.role,
            asset.backend,
            asset.platform,
            asset.version
        ));
    }
    Ok(())
}

fn archive_format_matches_platform(asset: &SemanticAssetMetadata) -> bool {
    (asset.platform == "windows-x64" && asset.archive_format == "zip")
        || (asset.platform != "windows-x64" && asset.archive_format == "tar.zst")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn hash(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn file(path: &str, size: u64, sha256: &str) -> Value {
        json!({"path": path, "sha256": sha256, "size": size})
    }

    struct AssetFixture<'a> {
        role: &'a str,
        backend: &'a str,
        version: &'a str,
        platform: &'a str,
        artifact: &'a str,
        archive_format: &'a str,
        archive_path_prefix: &'a str,
        files: Vec<Value>,
        archive_sha256: &'a str,
    }

    fn asset(fixture: AssetFixture<'_>) -> Value {
        let AssetFixture {
            role,
            backend,
            version,
            platform,
            artifact,
            archive_format,
            archive_path_prefix,
            files,
            archive_sha256,
        } = fixture;
        let expanded = files
            .iter()
            .map(|value| value["size"].as_u64().unwrap())
            .sum::<u64>();
        let max_files = files.len();
        json!({
            "archive_format": archive_format,
            "archive_path_prefix": archive_path_prefix,
            "archive_sha256": archive_sha256,
            "artifact": artifact,
            "backend": backend,
            "files": files,
            "max_expanded_bytes": expanded,
            "max_files": max_files,
            "platform": platform,
            "role": role,
            "version": version,
        })
    }

    fn model_files(accelerator: bool) -> Vec<Value> {
        let (onnx_size, onnx_sha) = if accelerator {
            (
                235_052_531,
                "4654c156f3e4171abc9c716cdb771bf9116455d15ac1aab364aeeede0e3205b0",
            )
        } else {
            (
                470_268_510,
                "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
            )
        };
        vec![
            file("LICENSE", 1, &hash('b')),
            file(
                "config.json",
                655,
                "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
            ),
            file("manifest.json", 1, &hash('c')),
            file("onnx/model.onnx", onnx_size, onnx_sha),
            file(
                "special_tokens_map.json",
                167,
                "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
            ),
            file(
                "tokenizer.json",
                17_082_730,
                "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
            ),
            file(
                "tokenizer_config.json",
                443,
                "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
            ),
        ]
    }

    fn authority(target: &str, backend: &str, asset_ids: Vec<&str>) -> Value {
        json!({
            "asset_ids": asset_ids,
            "backend": backend,
            "model_contract": {
                "dimensions": 384,
                "model_id": "intfloat/multilingual-e5-small",
                "normalization": "l2",
                "passage_prefix": "passage: ",
                "pooling": "attention_mask_mean",
                "query_prefix": "query: ",
                "revision": "614241f622f53c4eeff9890bdc4f31cfecc418b3",
            },
            "runtime_install_manifest_schema_version": 1,
            "schema_version": 1,
            "target": target,
        })
    }

    fn encode(value: &Value) -> String {
        BASE64.encode(serde_json::to_vec(value).unwrap())
    }

    fn valid_metadata() -> BTreeMap<String, String> {
        let mut assets = serde_json::Map::new();
        assets.insert(
            "onnx_model".to_owned(),
            asset(AssetFixture {
                role: "model",
                backend: "onnx",
                version: "1.0.0",
                platform: "any",
                artifact: CPU_MODEL_ARTIFACT,
                archive_format: "tar.xz",
                archive_path_prefix: "ctx-multilingual-e5-small-onnx-fp32-1.0.0",
                files: model_files(false),
                archive_sha256: &hash('a'),
            }),
        );
        assets.insert(
            "onnx_model_o4_fp16".to_owned(),
            asset(AssetFixture {
                role: "model",
                backend: "onnx",
                version: "1.0.0",
                platform: "any",
                artifact: ACCELERATOR_MODEL_ARTIFACT,
                archive_format: "tar.xz",
                archive_path_prefix: "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0",
                files: model_files(true),
                archive_sha256: &hash('a'),
            }),
        );
        for (id, platform, artifact) in [
            ("linux_x64_cpu", "linux-x64", "ctx-ort-linux-x64.tar.zst"),
            (
                "linux_aarch64_cpu",
                "linux-aarch64",
                "ctx-ort-linux-aarch64.tar.zst",
            ),
            (
                "macos_arm64_cpu",
                "macos-arm64",
                "ctx-ort-macos-arm64.tar.zst",
            ),
            ("macos_x64_cpu", "macos-x64", "ctx-ort-macos-x64.tar.zst"),
            (
                "freebsd_x64_cpu",
                "freebsd-x64",
                "ctx-ort-freebsd-x64.tar.zst",
            ),
        ] {
            assets.insert(
                id.to_owned(),
                asset(AssetFixture {
                    role: "cpu-runtime",
                    backend: "ort-cpu",
                    version: "1.27.0",
                    platform,
                    artifact,
                    archive_format: "tar.zst",
                    archive_path_prefix: "",
                    files: vec![file("lib/libonnxruntime.so", 1, &hash('d'))],
                    archive_sha256: &hash('a'),
                }),
            );
        }
        assets.insert(
            "windows_x64_windows_ml".to_owned(),
            asset(AssetFixture {
                role: "cpu-runtime",
                backend: "windows-ml",
                version: "2.1.74",
                platform: "windows-x64",
                artifact: WINDOWS_ML_ARTIFACT,
                archive_format: "zip",
                archive_path_prefix: "",
                files: WINDOWS_ML_FILES
                    .iter()
                    .map(|path| file(path, 1, &hash('d')))
                    .collect(),
                archive_sha256: &hash('a'),
            }),
        );
        assets.insert(
            "linux_x64_cuda".to_owned(),
            asset(AssetFixture {
                role: "accelerator",
                backend: "ort-cuda",
                version: "1.27.0",
                platform: "linux-x64-cuda12",
                artifact: "ctx-onnxruntime-linux-x64-cuda12.tar.zst",
                archive_format: "tar.zst",
                archive_path_prefix: "",
                files: vec![file("lib/libonnxruntime.so", 1, &hash('d'))],
                archive_sha256: &hash('a'),
            }),
        );
        assets.insert(
            "apple_coreml".to_owned(),
            asset(AssetFixture {
                role: "accelerator",
                backend: "coreml",
                version: "1.0.0",
                platform: "macos-arm64",
                artifact: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
                archive_format: "tar.xz",
                archive_path_prefix: "ctx-multilingual-e5-small-coreml-fp16-1.0.0",
                files: vec![file(
                    "manifest.json",
                    1,
                    "576c68756563333fdf442e6859f2392ca0065b09a2cb5d73983e30de75df1ad6",
                )],
                archive_sha256: "94c6fac5c4250079401d383adf1b10270fe5d370f2091dbad17bf4823222321e",
            }),
        );

        let mut metadata = BTreeMap::from([
            (SEMANTIC_SCHEMA_KEY.to_owned(), "1".to_owned()),
            (
                SEMANTIC_ASSETS_KEY.to_owned(),
                encode(&json!({"assets": assets, "schema_version": 1})),
            ),
        ]);
        for (key, value) in [
            (
                "apple_silicon_coreml",
                authority(
                    "apple-silicon",
                    "coreml",
                    vec!["onnx_model", "macos_arm64_cpu", "apple_coreml"],
                ),
            ),
            (
                "windows_windows_ml",
                authority(
                    "windows",
                    "windows-ml",
                    vec!["onnx_model_o4_fp16", "windows_x64_windows_ml"],
                ),
            ),
            (
                "linux_nvidia_ort_cuda",
                authority(
                    "linux-nvidia",
                    "ort-cuda",
                    vec!["onnx_model_o4_fp16", "linux_x64_cuda"],
                ),
            ),
            (
                "universal_ort_cpu",
                authority(
                    "universal",
                    "ort-cpu",
                    vec![
                        "onnx_model",
                        "linux_x64_cpu",
                        "linux_aarch64_cpu",
                        "macos_arm64_cpu",
                        "macos_x64_cpu",
                        "windows_x64_windows_ml",
                        "freebsd_x64_cpu",
                    ],
                ),
            ),
        ] {
            metadata.insert(
                format!("CTX_RELEASE_SEMANTIC_AUTHORITY_{key}"),
                encode(&value),
            );
        }
        metadata
    }

    #[test]
    fn accepts_canonical_catalog_and_selects_exact_cpu_pair() {
        let metadata = parse_semantic_metadata(&valid_metadata()).unwrap().unwrap();
        assert_eq!(metadata.assets.len(), 10);
        let selected = metadata.select("linux-x64", None).unwrap();
        assert_eq!(selected.target, "universal");
        assert_eq!(selected.backend, "ort-cpu");
        assert_eq!(
            selected
                .assets
                .iter()
                .map(|asset| asset.id.as_str())
                .collect::<Vec<_>>(),
            ["onnx_model", "linux_x64_cpu"]
        );
    }

    #[test]
    fn selects_exact_coreml_bundle_and_provisioned_cpu_fallback() {
        let metadata = parse_semantic_metadata(&valid_metadata()).unwrap().unwrap();
        let selected = metadata
            .select("macos-arm64", Some(SemanticAccelerator::CoreMl))
            .unwrap();
        assert_eq!(
            selected
                .assets
                .iter()
                .map(|asset| asset.id.as_str())
                .collect::<Vec<_>>(),
            ["onnx_model", "macos_arm64_cpu", "apple_coreml"]
        );
    }

    #[test]
    fn selects_exact_accelerator_only_windows_ml_and_cuda_pairs() {
        let metadata = parse_semantic_metadata(&valid_metadata()).unwrap().unwrap();
        for (platform, accelerator, expected) in [
            (
                "windows-x64",
                SemanticAccelerator::WindowsMl,
                ["onnx_model_o4_fp16", "windows_x64_windows_ml"],
            ),
            (
                "linux-x64",
                SemanticAccelerator::OrtCuda,
                ["onnx_model_o4_fp16", "linux_x64_cuda"],
            ),
        ] {
            let selected = metadata.select(platform, Some(accelerator)).unwrap();
            assert_eq!(
                selected
                    .assets
                    .iter()
                    .map(|asset| asset.id.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(selected.assets.iter().all(|asset| asset.id != "onnx_model"));
        }
    }

    #[test]
    fn rejects_coreml_authority_without_its_cpu_fallback_assets() {
        let mut metadata = valid_metadata();
        metadata.insert(
            "CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml".to_owned(),
            encode(&authority("apple-silicon", "coreml", vec!["apple_coreml"])),
        );
        let error = parse_semantic_metadata(&metadata).unwrap_err();
        assert!(format!("{error:#}").contains("macOS arm64 CPU runtime"));
    }

    #[test]
    fn rejects_noncanonical_signed_json_before_using_catalog() {
        let mut metadata = valid_metadata();
        let decoded = BASE64.decode(&metadata[SEMANTIC_ASSETS_KEY]).unwrap();
        let mut noncanonical = decoded;
        noncanonical.push(b'\n');
        metadata.insert(SEMANTIC_ASSETS_KEY.to_owned(), BASE64.encode(noncanonical));

        let error = parse_semantic_metadata(&metadata).unwrap_err();

        assert!(error.to_string().contains("not canonical JSON"));
    }
}
