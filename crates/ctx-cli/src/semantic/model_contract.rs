use std::{
    fmt,
    path::{Path, PathBuf},
};

pub(super) const SEMANTIC_BACKEND: &str = "multilingual-e5";
pub(super) const SEMANTIC_MODEL_KEY: &str = "e5-small-v1:mean-pool:l2:query-passage";
pub(super) const SEMANTIC_MODEL_ID: &str = ctx_history_index::SEMANTIC_EMBEDDING_MODEL;
pub(super) const SEMANTIC_MODEL_REVISION: &str =
    ctx_history_index::SEMANTIC_EMBEDDING_MODEL_REVISION;
pub(super) const SEMANTIC_MODEL_CONTRACT_VERSION: u32 =
    ctx_history_index::SEMANTIC_EMBEDDING_CONTRACT_REVISION;
pub(super) const SEMANTIC_HF_MODEL_CACHE_DIR: &str = "models--intfloat--multilingual-e5-small";
pub(super) const SEMANTIC_MANAGED_MODEL_CACHE_DIR: &str = "ctx-semantic-models";
const SEMANTIC_ACCELERATOR_ONNX_MODEL_FILE: SemanticModelFile = SemanticModelFile::new(
    "onnx/model.onnx",
    235_052_531,
    "4654c156f3e4171abc9c716cdb771bf9116455d15ac1aab364aeeede0e3205b0",
);
pub(super) const SEMANTIC_REQUIRED_MODEL_FILES: &[SemanticModelFile] = &[
    SemanticModelFile::new(
        "onnx/model.onnx",
        470_268_510,
        "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
    ),
    SemanticModelFile::new(
        "tokenizer.json",
        17_082_730,
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
    SemanticModelFile::new(
        "config.json",
        655,
        "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
    ),
    SemanticModelFile::new(
        "special_tokens_map.json",
        167,
        "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    ),
    SemanticModelFile::new(
        "tokenizer_config.json",
        443,
        "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    ),
];
#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(super) const SEMANTIC_PROVISIONING_MODEL_PATHS: &[&str] = &[
    "LICENSE",
    "config.json",
    "manifest.json",
    "onnx/model.onnx",
    "special_tokens_map.json",
    "tokenizer.json",
    "tokenizer_config.json",
];
pub(super) const SEMANTIC_DIMENSIONS: usize = ctx_history_index::SEMANTIC_EMBEDDING_DIMENSIONS;
pub(super) const SEMANTIC_MAX_SEQUENCE_LENGTH: usize = 512;
pub(super) const SEMANTIC_POOLING: &str = "attention-mask-mean";
pub(super) const SEMANTIC_NORMALIZATION: &str = ctx_history_index::SEMANTIC_EMBEDDING_NORMALIZATION;
pub(super) const SEMANTIC_PASSAGE_PREFIX: &str = "passage: ";
pub(super) const SEMANTIC_QUERY_PREFIX: &str = "query: ";
pub(super) const SEMANTIC_CONTRACT_CANARY_TEXT: &str = "búsqueda semántica 世界";
#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
const SEMANTIC_RELEASE_POOLING: &str = "attention_mask_mean";

const UNPROVISIONED_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) struct CoreMlBundleContract<'a> {
    pub artifact_url: &'a str,
    pub artifact_name: &'a str,
    pub archive_sha256: &'a str,
    pub manifest_sha256: &'a str,
    pub bundle_id: &'a str,
    pub bundle_version: &'a str,
    pub schema_version: u32,
    pub model_id: &'a str,
    pub source_revision: &'a str,
    pub embedding_space_id: &'a str,
    pub precision: &'a str,
    pub minimum_macos: &'a str,
    pub inputs: [(&'a str, &'a str); 3],
    pub output_name: &'a str,
    pub output_dtype: &'a str,
    pub document_batch_size: u32,
    pub query_batch_size: Option<u32>,
    pub max_sequence_length: u32,
    pub embedding_dimensions: u32,
    pub document_prefix: &'a str,
    pub query_prefix: &'a str,
    pub pooling: &'a str,
    pub normalization: &'a str,
    pub tokenizer_artifact: &'a str,
    pub document_model_artifact: &'a str,
    pub query_model_artifact: &'a str,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl CoreMlBundleContract<'_> {
    pub(super) fn provisioned(&self) -> bool {
        self.archive_sha256 != UNPROVISIONED_SHA256 && self.manifest_sha256 != UNPROVISIONED_SHA256
    }
}

// This is the sole authority for the signed Core ML artifact and every model
// value used to validate or publish it. Keep the descriptor inert with zero
// hashes until a replacement archive has been independently verified.
pub(super) const COREML_BUNDLE_CONTRACT: CoreMlBundleContract<'static> = CoreMlBundleContract {
    artifact_url: "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
    artifact_name: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
    archive_sha256: "94c6fac5c4250079401d383adf1b10270fe5d370f2091dbad17bf4823222321e",
    manifest_sha256: "576c68756563333fdf442e6859f2392ca0065b09a2cb5d73983e30de75df1ad6",
    bundle_id: "ctx.multilingual-e5-small.coreml.fp16",
    bundle_version: "1.0.0",
    schema_version: 1,
    model_id: SEMANTIC_MODEL_ID,
    source_revision: SEMANTIC_MODEL_REVISION,
    embedding_space_id: SEMANTIC_MODEL_KEY,
    precision: "fp16",
    minimum_macos: "13.0",
    inputs: [
        ("input_ids", "int32"),
        ("attention_mask", "int32"),
        ("token_type_ids", "int32"),
    ],
    output_name: "sentence_embeddings",
    output_dtype: "float32",
    document_batch_size: 16,
    query_batch_size: Some(1),
    max_sequence_length: 512,
    embedding_dimensions: SEMANTIC_DIMENSIONS as u32,
    document_prefix: SEMANTIC_PASSAGE_PREFIX,
    query_prefix: SEMANTIC_QUERY_PREFIX,
    pooling: "attention_mask_mean",
    normalization: "l2",
    tokenizer_artifact: "tokenizer.json",
    document_model_artifact: "document.mlpackage",
    query_model_artifact: "query.mlpackage",
};

#[derive(Clone, Copy, Debug)]
pub(super) struct SemanticModelFile {
    pub(super) path: &'static str,
    pub(super) size: u64,
    pub(super) sha256: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticBackendKind {
    Cpu,
    CoreMl,
    OrtCuda,
    WindowsMl,
}

impl SemanticBackendKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::CoreMl => "coreml",
            Self::OrtCuda => "ort_cuda",
            Self::WindowsMl => "windows_ml",
        }
    }

    pub(super) fn execution_provider(self) -> &'static str {
        match self {
            Self::Cpu => "CPUExecutionProvider",
            Self::CoreMl => "CoreML",
            Self::OrtCuda => "CUDAExecutionProvider",
            Self::WindowsMl => "WindowsML:DmlExecutionProvider:GPU",
        }
    }

    pub(super) fn contract_id(self) -> String {
        match self {
            Self::Cpu => "ort-cpu:1.27.0:fastembed-5.17.2:cpu-only-v1".to_owned(),
            Self::CoreMl => format!(
                "coreml-native-0.2.0:{}:{}:{}",
                COREML_BUNDLE_CONTRACT.bundle_id,
                COREML_BUNDLE_CONTRACT.bundle_version,
                COREML_BUNDLE_CONTRACT.manifest_sha256,
            ),
            Self::OrtCuda => "ort-adapter:CUDAExecutionProvider:v1".to_owned(),
            Self::WindowsMl => "windows-ml:2.1.74:ort-1.24.6:included-dml-gpu-v1".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticOrtModelVariant {
    CpuFp32,
    AcceleratorO4Fp16,
}

impl SemanticOrtModelVariant {
    pub(super) fn for_backend(backend: SemanticBackendKind) -> Self {
        match backend {
            SemanticBackendKind::Cpu => Self::CpuFp32,
            SemanticBackendKind::OrtCuda | SemanticBackendKind::WindowsMl => {
                Self::AcceleratorO4Fp16
            }
            SemanticBackendKind::CoreMl => {
                unreachable!("Core ML does not load the managed ONNX model")
            }
        }
    }

    pub(super) fn required_files(self) -> impl Iterator<Item = SemanticModelFile> + Clone {
        let model = match self {
            Self::CpuFp32 => SEMANTIC_REQUIRED_MODEL_FILES[0],
            Self::AcceleratorO4Fp16 => SEMANTIC_ACCELERATOR_ONNX_MODEL_FILE,
        };
        std::iter::once(model).chain(SEMANTIC_REQUIRED_MODEL_FILES[1..].iter().copied())
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::CpuFp32 => "cpu-fp32",
            Self::AcceleratorO4Fp16 => "accelerator-o4-fp16",
        }
    }
}

impl SemanticModelFile {
    pub(super) const fn new(path: &'static str, size: u64, sha256: &'static str) -> Self {
        Self { path, size, sha256 }
    }
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_managed_model_snapshot_dir(cache_dir: &Path) -> PathBuf {
    cache_dir
        .join(SEMANTIC_MANAGED_MODEL_CACHE_DIR)
        .join(SEMANTIC_HF_MODEL_CACHE_DIR)
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION)
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_provisioning_model_contract_matches(
    model_id: &str,
    revision: &str,
    dimensions: u32,
    pooling: &str,
    normalization: &str,
    query_prefix: &str,
    passage_prefix: &str,
) -> bool {
    model_id == SEMANTIC_MODEL_ID
        && revision == SEMANTIC_MODEL_REVISION
        && dimensions == SEMANTIC_DIMENSIONS as u32
        && pooling == SEMANTIC_RELEASE_POOLING
        && normalization == SEMANTIC_NORMALIZATION
        && query_prefix == SEMANTIC_QUERY_PREFIX
        && passage_prefix == SEMANTIC_PASSAGE_PREFIX
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_provisioning_model_path_count() -> usize {
    SEMANTIC_PROVISIONING_MODEL_PATHS.len()
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_provisioning_model_path_matches(path: &str) -> bool {
    SEMANTIC_PROVISIONING_MODEL_PATHS.contains(&path)
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_required_model_file_count(variant: SemanticOrtModelVariant) -> usize {
    variant.required_files().count()
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_required_model_file_matches(
    variant: SemanticOrtModelVariant,
    path: &str,
    size: u64,
    sha256: &str,
) -> bool {
    variant
        .required_files()
        .any(|file| file.path == path && file.size == size && file.sha256 == sha256)
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub(crate) fn semantic_provisioning_coreml_asset_matches(
    artifact: &str,
    archive_sha256: &str,
    manifest_sha256: &str,
) -> bool {
    COREML_BUNDLE_CONTRACT.provisioned()
        && artifact == COREML_BUNDLE_CONTRACT.artifact_name
        && archive_sha256 == COREML_BUNDLE_CONTRACT.archive_sha256
        && manifest_sha256 == COREML_BUNDLE_CONTRACT.manifest_sha256
}

#[derive(Debug)]
pub(super) struct SemanticCpuModelIntegrityError(pub(super) String);

impl fmt::Display for SemanticCpuModelIntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SemanticCpuModelIntegrityError {}

#[derive(Debug)]
pub(super) struct SemanticCpuModelCacheMissing(pub(super) String);

impl fmt::Display for SemanticCpuModelCacheMissing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SemanticCpuModelCacheMissing {}

#[derive(Debug)]
pub(super) struct SemanticModelLoadDeferred {
    pub(super) available_memory_bytes: u64,
    pub(super) required_available_memory_bytes: u64,
}

impl fmt::Display for SemanticModelLoadDeferred {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "semantic CPU model load deferred: {} bytes available, {} required",
            self.available_memory_bytes, self.required_available_memory_bytes
        )
    }
}

impl std::error::Error for SemanticModelLoadDeferred {}

#[derive(Debug)]
pub(super) struct SemanticProvisioningRequired {
    pub(super) asset: &'static str,
    pub(super) detail: String,
}

impl fmt::Display for SemanticProvisioningRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "semantic asset {} requires provisioning by the official installer or repair flow: {}",
            self.asset, self.detail
        )
    }
}

impl std::error::Error for SemanticProvisioningRequired {}

pub(super) fn semantic_model_contract_descriptor() -> String {
    let mut descriptor = format!(
        "ctx-semantic-e5-v{}|model={SEMANTIC_MODEL_ID}|revision={SEMANTIC_MODEL_REVISION}|dimensions={SEMANTIC_DIMENSIONS}|max_sequence_length={SEMANTIC_MAX_SEQUENCE_LENGTH}|pooling={SEMANTIC_POOLING}|normalization={SEMANTIC_NORMALIZATION}|query_prefix={SEMANTIC_QUERY_PREFIX}|passage_prefix={SEMANTIC_PASSAGE_PREFIX}|language=unicode-global",
        SEMANTIC_MODEL_CONTRACT_VERSION,
    );
    for variant in [
        SemanticOrtModelVariant::CpuFp32,
        SemanticOrtModelVariant::AcceleratorO4Fp16,
    ] {
        for file in variant.required_files() {
            use std::fmt::Write as _;
            write!(
                descriptor,
                "|variant={}|file={}:{}:{}",
                variant.as_str(),
                file.path,
                file.size,
                file.sha256
            )
            .expect("writing to String cannot fail");
        }
    }
    descriptor
}

pub(super) fn semantic_model_key() -> &'static str {
    SEMANTIC_MODEL_KEY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioning_inventory_keeps_both_pinned_e5_variants_exact() {
        assert_eq!(
            semantic_required_model_file_count(SemanticOrtModelVariant::CpuFp32),
            5
        );
        assert_eq!(
            semantic_required_model_file_count(SemanticOrtModelVariant::AcceleratorO4Fp16),
            5
        );
        assert_eq!(semantic_provisioning_model_path_count(), 7);
        assert!(SemanticOrtModelVariant::CpuFp32
            .required_files()
            .all(|file| {
                semantic_provisioning_model_path_matches(file.path)
                    && semantic_required_model_file_matches(
                        SemanticOrtModelVariant::CpuFp32,
                        file.path,
                        file.size,
                        file.sha256,
                    )
            }));
        assert!(SemanticOrtModelVariant::AcceleratorO4Fp16
            .required_files()
            .all(|file| {
                semantic_provisioning_model_path_matches(file.path)
                    && semantic_required_model_file_matches(
                        SemanticOrtModelVariant::AcceleratorO4Fp16,
                        file.path,
                        file.size,
                        file.sha256,
                    )
            }));
        assert_eq!(
            SEMANTIC_PROVISIONING_MODEL_PATHS
                .iter()
                .copied()
                .filter(|path| {
                    !SemanticOrtModelVariant::CpuFp32
                        .required_files()
                        .any(|file| file.path == *path)
                })
                .collect::<Vec<_>>(),
            ["LICENSE", "manifest.json"]
        );
    }
}
