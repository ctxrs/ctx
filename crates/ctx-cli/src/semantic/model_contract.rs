use std::fmt;

pub(super) const SEMANTIC_BACKEND: &str = "multilingual-e5";
pub(super) const SEMANTIC_MODEL_KEY: &str = "e5-small-v1:mean-pool:l2:query-passage";
pub(super) const SEMANTIC_MODEL_ID: &str = "intfloat/multilingual-e5-small";
pub(super) const SEMANTIC_MODEL_REVISION: &str = "614241f622f53c4eeff9890bdc4f31cfecc418b3";
pub(super) const SEMANTIC_HF_MODEL_CACHE_DIR: &str = "models--intfloat--multilingual-e5-small";
pub(super) const SEMANTIC_MANAGED_MODEL_CACHE_DIR: &str = "ctx-semantic-models";
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
pub(super) const SEMANTIC_DIMENSIONS: usize = 384;
pub(super) const SEMANTIC_PASSAGE_PREFIX: &str = "passage: ";
pub(super) const SEMANTIC_QUERY_PREFIX: &str = "query: ";

#[cfg(any(target_os = "macos", test))]
const UNPROVISIONED_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy)]
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

#[cfg(any(target_os = "macos", test))]
impl CoreMlBundleContract<'_> {
    pub(super) fn provisioned(&self) -> bool {
        self.archive_sha256 != UNPROVISIONED_SHA256 && self.manifest_sha256 != UNPROVISIONED_SHA256
    }
}

// This is the sole authority for the signed Core ML artifact and every model
// value used to validate or publish it. Keep the descriptor inert with zero
// hashes until a replacement archive has been independently verified.
#[cfg(any(target_os = "macos", test))]
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

impl SemanticModelFile {
    pub(super) const fn new(path: &'static str, size: u64, sha256: &'static str) -> Self {
        Self { path, size, sha256 }
    }
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

pub(super) fn semantic_model_key() -> &'static str {
    SEMANTIC_MODEL_KEY
}
