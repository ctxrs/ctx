use std::fmt;

pub const SEMANTIC_BACKEND: &str = "multilingual-e5";
pub const SEMANTIC_MODEL_KEY: &str = "e5-small-v1:mean-pool:l2:query-passage";
pub const SEMANTIC_MODEL_ID: &str = "intfloat/multilingual-e5-small";
pub const SEMANTIC_MODEL_REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
pub const SEMANTIC_MODEL_CONTRACT_VERSION: u32 = 2;
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
pub const SEMANTIC_DIMENSIONS: usize = 384;
pub(super) const SEMANTIC_MAX_SEQUENCE_LENGTH: usize = 512;
pub const SEMANTIC_POOLING: &str = "attention-mask-mean";
pub const SEMANTIC_NORMALIZATION: &str = "l2";
pub const SEMANTIC_PASSAGE_PREFIX: &str = "passage: ";
pub const SEMANTIC_QUERY_PREFIX: &str = "query: ";
pub(super) const SEMANTIC_CONTRACT_CANARY_TEXT: &str = "búsqueda semántica 世界";
#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
const SEMANTIC_RELEASE_POOLING: &str = "attention_mask_mean";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticModelContract {
    model_id: &'static str,
    model_revision: &'static str,
    contract_revision: u32,
    dimensions: usize,
    normalization: &'static str,
}

impl SemanticModelContract {
    pub const fn model_id(self) -> &'static str {
        self.model_id
    }

    pub const fn model_revision(self) -> &'static str {
        self.model_revision
    }

    pub const fn contract_revision(self) -> u32 {
        self.contract_revision
    }

    pub const fn dimensions(self) -> usize {
        self.dimensions
    }

    pub const fn normalization(self) -> &'static str {
        self.normalization
    }
}

pub const fn semantic_model_contract() -> SemanticModelContract {
    SemanticModelContract {
        model_id: SEMANTIC_MODEL_ID,
        model_revision: SEMANTIC_MODEL_REVISION,
        contract_revision: SEMANTIC_MODEL_CONTRACT_VERSION,
        dimensions: SEMANTIC_DIMENSIONS,
        normalization: SEMANTIC_NORMALIZATION,
    }
}

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
    max_sequence_length: SEMANTIC_MAX_SEQUENCE_LENGTH as u32,
    embedding_dimensions: SEMANTIC_DIMENSIONS as u32,
    document_prefix: SEMANTIC_PASSAGE_PREFIX,
    query_prefix: SEMANTIC_QUERY_PREFIX,
    pooling: SEMANTIC_RELEASE_POOLING,
    normalization: SEMANTIC_NORMALIZATION,
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
pub enum SemanticOrtModelVariant {
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

    pub fn as_str(self) -> &'static str {
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
pub fn semantic_provisioning_model_contract_matches(
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
pub fn semantic_provisioning_model_path_count() -> usize {
    SEMANTIC_PROVISIONING_MODEL_PATHS.len()
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub fn semantic_provisioning_model_path_matches(path: &str) -> bool {
    SEMANTIC_PROVISIONING_MODEL_PATHS.contains(&path)
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub fn semantic_required_model_file_count(variant: SemanticOrtModelVariant) -> usize {
    variant.required_files().count()
}

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub fn semantic_required_model_file_matches(
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
pub fn semantic_provisioning_coreml_asset_matches(
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
pub struct SemanticModelLoadDeferred {
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

impl SemanticModelLoadDeferred {
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(available_memory_bytes: u64, required_available_memory_bytes: u64) -> Self {
        Self {
            available_memory_bytes,
            required_available_memory_bytes,
        }
    }

    pub fn available_memory_bytes(&self) -> u64 {
        self.available_memory_bytes
    }

    pub fn required_available_memory_bytes(&self) -> u64 {
        self.required_available_memory_bytes
    }
}

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

pub fn semantic_model_contract_descriptor() -> String {
    let mut descriptor = format!(
        "ctx-semantic-e5-v{}|backend={SEMANTIC_BACKEND}|model_key={SEMANTIC_MODEL_KEY}|model={SEMANTIC_MODEL_ID}|revision={SEMANTIC_MODEL_REVISION}|dimensions={SEMANTIC_DIMENSIONS}|max_sequence_length={SEMANTIC_MAX_SEQUENCE_LENGTH}|pooling={SEMANTIC_POOLING}|normalization={SEMANTIC_NORMALIZATION}|query_prefix={SEMANTIC_QUERY_PREFIX}|passage_prefix={SEMANTIC_PASSAGE_PREFIX}|language=unicode-global",
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
    for backend in [
        SemanticBackendKind::Cpu,
        SemanticBackendKind::CoreMl,
        SemanticBackendKind::OrtCuda,
        SemanticBackendKind::WindowsMl,
    ] {
        use std::fmt::Write as _;
        write!(
            descriptor,
            "|backend_variant={}:{}:{}",
            backend.as_str(),
            backend.execution_provider(),
            backend.contract_id(),
        )
        .expect("writing to String cannot fail");
    }
    let coreml = COREML_BUNDLE_CONTRACT;
    use std::fmt::Write as _;
    write!(
        descriptor,
        "|coreml_artifact_url={}|coreml_artifact_name={}|coreml_archive_sha256={}|coreml_manifest_sha256={}|coreml_bundle_id={}|coreml_bundle_version={}|coreml_schema_version={}|coreml_model_id={}|coreml_source_revision={}|coreml_embedding_space_id={}|coreml_precision={}|coreml_minimum_macos={}|coreml_inputs={}:{};{}:{};{}:{}|coreml_output={}:{}|coreml_document_batch_size={}|coreml_query_batch_size={:?}|coreml_max_sequence_length={}|coreml_dimensions={}|coreml_document_prefix={}|coreml_query_prefix={}|coreml_pooling={}|coreml_normalization={}|coreml_tokenizer_artifact={}|coreml_document_model_artifact={}|coreml_query_model_artifact={}",
        coreml.artifact_url,
        coreml.artifact_name,
        coreml.archive_sha256,
        coreml.manifest_sha256,
        coreml.bundle_id,
        coreml.bundle_version,
        coreml.schema_version,
        coreml.model_id,
        coreml.source_revision,
        coreml.embedding_space_id,
        coreml.precision,
        coreml.minimum_macos,
        coreml.inputs[0].0,
        coreml.inputs[0].1,
        coreml.inputs[1].0,
        coreml.inputs[1].1,
        coreml.inputs[2].0,
        coreml.inputs[2].1,
        coreml.output_name,
        coreml.output_dtype,
        coreml.document_batch_size,
        coreml.query_batch_size,
        coreml.max_sequence_length,
        coreml.embedding_dimensions,
        coreml.document_prefix,
        coreml.query_prefix,
        coreml.pooling,
        coreml.normalization,
        coreml.tokenizer_artifact,
        coreml.document_model_artifact,
        coreml.query_model_artifact,
    )
    .expect("writing to String cannot fail");
    descriptor
}

pub fn semantic_model_key() -> &'static str {
    SEMANTIC_MODEL_KEY
}

pub fn semantic_tokenizer_fingerprint() -> String {
    SEMANTIC_REQUIRED_MODEL_FILES
        .iter()
        .find(|file| file.path == "tokenizer.json")
        .map(|file| format!("sha256:{}", file.sha256))
        .unwrap_or_else(|| "missing-tokenizer-contract".to_owned())
}

fn semantic_e5_prefixed_text(prefix: &str, text: &str) -> String {
    let text = text.trim_start();
    if text.starts_with(prefix) {
        text.to_owned()
    } else {
        format!("{prefix}{text}")
    }
}

pub fn semantic_e5_passage_text(text: &str) -> String {
    semantic_e5_prefixed_text(SEMANTIC_PASSAGE_PREFIX, text)
}

pub(super) fn semantic_e5_query_text(text: &str) -> String {
    semantic_e5_prefixed_text(SEMANTIC_QUERY_PREFIX, text)
}
