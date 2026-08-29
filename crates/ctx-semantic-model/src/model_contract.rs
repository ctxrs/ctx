use std::{fmt, sync::OnceLock};

use anyhow::{anyhow, Result};
use sha2::{Digest, Sha256};

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
pub const BUILTIN_SEMANTIC_EXECUTOR_ROUTE_IDENTITY: &str = "builtin";
pub const MAX_EXTERNAL_SEMANTIC_DIMENSIONS: usize = 4_096;
pub const MAX_EXTERNAL_SEMANTIC_SPACE_ID_BYTES: usize = 256;
const MAX_EXTERNAL_SEMANTIC_INPUTS_PER_REQUEST: usize = 512;
const MAX_EXTERNAL_SEMANTIC_SCALARS_PER_REQUEST: usize = 262_144;
pub(super) const SEMANTIC_MAX_SEQUENCE_LENGTH: usize = 512;
pub const SEMANTIC_POOLING: &str = "attention-mask-mean";
pub const SEMANTIC_NORMALIZATION: &str = "l2";
pub const SEMANTIC_PASSAGE_PREFIX: &str = "passage: ";
pub const SEMANTIC_QUERY_PREFIX: &str = "query: ";
pub const SEMANTIC_LANGUAGE_SCOPE: &str = "unicode-global";
pub(super) const SEMANTIC_CONTRACT_CANARY_TEXT: &str = "búsqueda semántica 世界";
pub(super) const SEMANTIC_TOKENIZER_BEHAVIOR_PATHS: &[&str] = &[
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];
const LEGACY_BUILTIN_DESCRIPTOR_FINGERPRINT: &str =
    "sha256:c812eb325bc5e90e7278b2b8da3933206340c5b5a46fd678be40016e06a89fc3";
const LEGACY_COMPATIBLE_VECTOR_CONTRACT_FINGERPRINT: &str =
    "sha256:611f11c9b715543137d1b6be8d87497a2b6ef4945d425f3c0b973d2cb0c6036d";
#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
const SEMANTIC_RELEASE_POOLING: &str = "attention_mask_mean";

/// The endpoint-declared compatibility identity of an external semantic space.
///
/// `space_id` is opaque to ctx and must be globally unique for one compatible
/// coordinate system. Endpoints must change it whenever preprocessing,
/// tokenization, model behavior, or any other detail affecting produced vectors
/// becomes incompatible. The value uses a conservative ASCII model-ID alphabet
/// so it is safe to persist, log, and compare exactly across protocol boundaries.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ExternalSemanticSpace {
    space_id: String,
    dimensions: usize,
}

impl ExternalSemanticSpace {
    pub fn new(space_id: impl Into<String>, dimensions: usize) -> Result<Self> {
        let space_id = space_id.into();
        if space_id.is_empty()
            || space_id.len() > MAX_EXTERNAL_SEMANTIC_SPACE_ID_BYTES
            || !space_id.bytes().all(is_header_safe_space_id_byte)
        {
            return Err(anyhow!(
                "external semantic space ID must use safe ASCII model-ID characters and be at most {MAX_EXTERNAL_SEMANTIC_SPACE_ID_BYTES} bytes"
            ));
        }
        if !(1..=MAX_EXTERNAL_SEMANTIC_DIMENSIONS).contains(&dimensions) {
            return Err(anyhow!(
                "external semantic space dimensions must be between 1 and {MAX_EXTERNAL_SEMANTIC_DIMENSIONS}"
            ));
        }
        Ok(Self {
            space_id,
            dimensions,
        })
    }

    pub fn space_id(&self) -> &str {
        &self.space_id
    }

    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Maximum inputs in one external embedding request or index work unit.
    /// This caps both input cardinality and total vector scalars.
    pub const fn max_inputs_per_request(&self) -> usize {
        let scalar_limited = MAX_EXTERNAL_SEMANTIC_SCALARS_PER_REQUEST / self.dimensions;
        if scalar_limited < MAX_EXTERNAL_SEMANTIC_INPUTS_PER_REQUEST {
            scalar_limited
        } else {
            MAX_EXTERNAL_SEMANTIC_INPUTS_PER_REQUEST
        }
    }
}

fn is_header_safe_space_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(byte, b'.' | b'_' | b':' | b'/' | b'@' | b'+' | b'=' | b'-')
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExternalSemanticContractIdentity {
    normalized_endpoint: String,
    space: ExternalSemanticSpace,
}

/// Query text prepared according to one semantic vector-space contract.
#[derive(Clone, Eq, PartialEq)]
pub struct PreparedSemanticQuery {
    contract_fingerprint: String,
    text: String,
}

impl PreparedSemanticQuery {
    pub fn contract_fingerprint(&self) -> &str {
        &self.contract_fingerprint
    }

    pub fn into_text(self) -> String {
        self.text
    }
}

/// Document texts prepared according to one semantic vector-space contract.
#[derive(Clone, Eq, PartialEq)]
pub struct PreparedSemanticDocuments {
    contract_fingerprint: String,
    texts: Vec<String>,
}

impl PreparedSemanticDocuments {
    pub fn contract_fingerprint(&self) -> &str {
        &self.contract_fingerprint
    }

    pub fn into_texts(self) -> Vec<String> {
        self.texts
    }
}

/// The complete compatibility identity of one semantic vector space.
///
/// Builtin identity excludes executor, runtime, accelerator, and artifact
/// publication details while retaining the complete pinned E5 compatibility
/// contract. An external vector space is identified by the endpoint-declared
/// space and dimensions; the endpoint remains runtime routing and fencing
/// state rather than part of vector compatibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticModelContract {
    model_key: String,
    model_id: String,
    model_revision: String,
    contract_version: u32,
    tokenizer_fingerprint: String,
    tokenizer_behavior_fingerprint: String,
    dimensions: usize,
    max_sequence_length: usize,
    pooling: String,
    normalization: String,
    query_prefix: String,
    document_prefix: String,
    language_scope: String,
    descriptor: String,
    fingerprint: String,
    external: Option<ExternalSemanticContractIdentity>,
}

impl SemanticModelContract {
    pub(crate) fn external_http(normalized_endpoint: &str, space: ExternalSemanticSpace) -> Self {
        let mut contract = Self {
            model_key: space.space_id().to_owned(),
            model_id: "external-http-v1".to_owned(),
            model_revision: space.space_id().to_owned(),
            contract_version: 1,
            tokenizer_fingerprint: "endpoint-owned-v1".to_owned(),
            tokenizer_behavior_fingerprint: "endpoint-owned-v1".to_owned(),
            dimensions: space.dimensions(),
            max_sequence_length: 0,
            pooling: "endpoint-owned".to_owned(),
            normalization: SEMANTIC_NORMALIZATION.to_owned(),
            query_prefix: String::new(),
            document_prefix: String::new(),
            language_scope: "endpoint-owned".to_owned(),
            descriptor: String::new(),
            fingerprint: String::new(),
            external: Some(ExternalSemanticContractIdentity {
                normalized_endpoint: normalized_endpoint.to_owned(),
                space,
            }),
        };
        contract.rebuild_identity();
        contract
    }

    /// Returns the endpoint-declared space for an external HTTP contract.
    pub fn external_space(&self) -> Option<&ExternalSemanticSpace> {
        self.external.as_ref().map(|identity| &identity.space)
    }

    /// Returns the normalized runtime endpoint associated with this contract.
    pub fn external_http_endpoint(&self) -> Option<&str> {
        self.external
            .as_ref()
            .map(|identity| identity.normalized_endpoint.as_str())
    }

    /// Identifies the configured executor route independently of vector-space
    /// compatibility. External identities are derived from the normalized
    /// endpoint; the built-in executor uses a fixed sentinel.
    pub fn executor_route_identity(&self) -> String {
        match self.external_http_endpoint() {
            Some(endpoint) => sha256_fingerprint(&format!(
                "ctx-semantic-executor-route-v1|endpoint_bytes={}|endpoint={endpoint}",
                endpoint.len(),
            )),
            None => BUILTIN_SEMANTIC_EXECUTOR_ROUTE_IDENTITY.to_owned(),
        }
    }

    pub fn model_key(&self) -> &str {
        &self.model_key
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn model_revision(&self) -> &str {
        &self.model_revision
    }

    pub const fn contract_version(&self) -> u32 {
        self.contract_version
    }

    pub const fn contract_revision(&self) -> u32 {
        self.contract_version()
    }

    pub fn tokenizer_fingerprint(&self) -> &str {
        &self.tokenizer_fingerprint
    }

    /// Identifies every pinned file that can alter fastembed tokenization.
    ///
    /// `tokenizer_fingerprint` intentionally remains the historical
    /// tokenizer.json-only Flat-format field. New vector-space compatibility
    /// must use this complete behavior fingerprint through `descriptor`.
    pub fn tokenizer_behavior_fingerprint(&self) -> &str {
        &self.tokenizer_behavior_fingerprint
    }

    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub const fn max_sequence_length(&self) -> usize {
        self.max_sequence_length
    }

    pub fn pooling(&self) -> &str {
        &self.pooling
    }

    pub fn normalization(&self) -> &str {
        &self.normalization
    }

    pub fn query_prefix(&self) -> &str {
        &self.query_prefix
    }

    pub fn document_prefix(&self) -> &str {
        &self.document_prefix
    }

    pub fn language_scope(&self) -> &str {
        &self.language_scope
    }

    /// Prepares query input for this vector space exactly once.
    pub fn prepare_query(&self, text: String) -> PreparedSemanticQuery {
        PreparedSemanticQuery {
            contract_fingerprint: self.fingerprint().to_owned(),
            text: if self.external.is_some() {
                text
            } else {
                semantic_prefixed_text(self.query_prefix(), text)
            },
        }
    }

    /// Prepares document inputs for this vector space exactly once.
    pub fn prepare_documents(&self, texts: Vec<String>) -> PreparedSemanticDocuments {
        PreparedSemanticDocuments {
            contract_fingerprint: self.fingerprint().to_owned(),
            texts: if self.external.is_some() {
                texts
            } else {
                texts
                    .into_iter()
                    .map(|text| semantic_prefixed_text(self.document_prefix(), text))
                    .collect()
            },
        }
    }

    /// Returns query text prepared for this vector space.
    pub fn query_text(&self, text: &str) -> String {
        self.prepare_query(text.to_owned()).into_text()
    }

    /// Returns document text prepared for this vector space.
    pub fn document_text(&self, text: &str) -> String {
        if self.external.is_some() {
            text.to_owned()
        } else {
            semantic_prefixed_text(self.document_prefix(), text.to_owned())
        }
    }

    /// Returns the canonical compatibility descriptor for this vector space.
    pub fn descriptor(&self) -> &str {
        &self.descriptor
    }

    fn rebuild_identity(&mut self) {
        if let Some(identity) = &self.external {
            self.descriptor = format!(
                "ctx-semantic-external-space-v1|space_id_bytes={}|space_id={}|dimensions={}",
                identity.space.space_id().len(),
                identity.space.space_id(),
                identity.space.dimensions(),
            );
            self.fingerprint = sha256_fingerprint(&self.descriptor);
            return;
        }
        self.descriptor = format!(
            "ctx-semantic-vector-space-v{}|model_key={}|model_id={}|model_revision={}|tokenizer_fingerprint={}|tokenizer_behavior_fingerprint={}|dimensions={}|max_sequence_length={}|pooling={}|normalization={}|query_prefix={}|document_prefix={}|language_scope={}",
            self.contract_version,
            self.model_key,
            self.model_id,
            self.model_revision,
            self.tokenizer_fingerprint,
            self.tokenizer_behavior_fingerprint,
            self.dimensions,
            self.max_sequence_length,
            self.pooling,
            self.normalization,
            self.query_prefix,
            self.document_prefix,
            self.language_scope,
        );
        self.fingerprint = sha256_fingerprint(&self.descriptor);
    }

    /// Returns the canonical SHA-256 identity of this vector space.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Reports whether frozen fingerprint-less daemon V1 compatibility is safe.
    pub fn supports_frozen_legacy_v1(&self) -> bool {
        let builtin = builtin_semantic_model_contract();
        self.fingerprint() == LEGACY_COMPATIBLE_VECTOR_CONTRACT_FINGERPRINT
            && (std::ptr::eq(self, builtin) || self == builtin)
    }

    /// Returns the exact pre-refactor built-in descriptor for index migration.
    ///
    /// The alias is available only for the exact built-in vector contract and
    /// only while reconstruction still matches the descriptor digest shipped
    /// before executor identity was separated from vector-space identity.
    pub fn legacy_builtin_descriptor_alias(&self) -> Option<&'static str> {
        if !self.supports_frozen_legacy_v1() {
            return None;
        }
        static ALIAS: OnceLock<Option<String>> = OnceLock::new();
        ALIAS
            .get_or_init(|| {
                let descriptor = legacy_builtin_semantic_model_descriptor();
                (sha256_fingerprint(&descriptor) == LEGACY_BUILTIN_DESCRIPTOR_FINGERPRINT)
                    .then_some(descriptor)
            })
            .as_deref()
    }

    #[cfg(test)]
    pub(super) fn with_test_tokenizer_behavior_fingerprint(mut self, fingerprint: &str) -> Self {
        self.tokenizer_behavior_fingerprint = fingerprint.to_owned();
        self.rebuild_identity();
        self
    }

    #[cfg(test)]
    pub(super) fn with_test_language_scope(mut self, language_scope: &str) -> Self {
        self.language_scope = language_scope.to_owned();
        self.rebuild_identity();
        self
    }
}

pub fn semantic_model_contract() -> &'static SemanticModelContract {
    builtin_semantic_model_contract()
}

fn builtin_semantic_model_contract() -> &'static SemanticModelContract {
    static CONTRACT: OnceLock<SemanticModelContract> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        let mut contract = SemanticModelContract {
            model_key: SEMANTIC_MODEL_KEY.to_owned(),
            model_id: SEMANTIC_MODEL_ID.to_owned(),
            model_revision: SEMANTIC_MODEL_REVISION.to_owned(),
            contract_version: SEMANTIC_MODEL_CONTRACT_VERSION,
            tokenizer_fingerprint: semantic_tokenizer_fingerprint(),
            tokenizer_behavior_fingerprint: semantic_tokenizer_behavior_fingerprint(),
            dimensions: SEMANTIC_DIMENSIONS,
            max_sequence_length: SEMANTIC_MAX_SEQUENCE_LENGTH,
            pooling: SEMANTIC_POOLING.to_owned(),
            normalization: SEMANTIC_NORMALIZATION.to_owned(),
            query_prefix: SEMANTIC_QUERY_PREFIX.to_owned(),
            document_prefix: SEMANTIC_PASSAGE_PREFIX.to_owned(),
            language_scope: SEMANTIC_LANGUAGE_SCOPE.to_owned(),
            descriptor: String::new(),
            fingerprint: String::new(),
            external: None,
        };
        contract.rebuild_identity();
        contract
    })
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

// This is the sole runtime authority for the signed Core ML publication and
// every model value used to validate it. Keep the descriptor inert with zero
// hashes until a replacement archive has been independently verified.
pub(super) const COREML_BUNDLE_CONTRACT: CoreMlBundleContract<'static> = CoreMlBundleContract {
    artifact_url: "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/stable/1.0.0/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
    artifact_name: "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
    archive_sha256: "25fbf333d1e72f5c075973ef968dfa1446459f61f3ac63ef3690d9865435af17",
    manifest_sha256: "20a94162aca7c2f9f65be27839cd6867ec1c54e142fdf0c652de20139dffbc19",
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
    builtin_semantic_model_contract().descriptor().to_owned()
}

pub fn semantic_model_contract_fingerprint() -> String {
    builtin_semantic_model_contract().fingerprint().to_owned()
}

fn legacy_builtin_semantic_model_descriptor() -> String {
    let mut descriptor = format!(
        "ctx-semantic-e5-v{}|backend={SEMANTIC_BACKEND}|model_key={SEMANTIC_MODEL_KEY}|model={SEMANTIC_MODEL_ID}|revision={SEMANTIC_MODEL_REVISION}|dimensions={SEMANTIC_DIMENSIONS}|max_sequence_length={SEMANTIC_MAX_SEQUENCE_LENGTH}|pooling={SEMANTIC_POOLING}|normalization={SEMANTIC_NORMALIZATION}|query_prefix={SEMANTIC_QUERY_PREFIX}|passage_prefix={SEMANTIC_PASSAGE_PREFIX}|language=unicode-global",
        SEMANTIC_MODEL_CONTRACT_VERSION,
    );
    append_builtin_semantic_executor_identity(&mut descriptor);
    descriptor
}

fn append_builtin_semantic_executor_identity(descriptor: &mut String) {
    for variant in [
        SemanticOrtModelVariant::CpuFp32,
        SemanticOrtModelVariant::AcceleratorO4Fp16,
    ] {
        for file in variant.required_files() {
            descriptor.push_str(&format!(
                "|variant={}|file={}:{}:{}",
                variant.as_str(),
                file.path,
                file.size,
                file.sha256
            ));
        }
    }
    for backend in [
        SemanticBackendKind::Cpu,
        SemanticBackendKind::CoreMl,
        SemanticBackendKind::OrtCuda,
        SemanticBackendKind::WindowsMl,
    ] {
        descriptor.push_str(&format!(
            "|backend_variant={}:{}:{}",
            backend.as_str(),
            backend.execution_provider(),
            backend.contract_id(),
        ));
    }
    let coreml = COREML_BUNDLE_CONTRACT;
    descriptor.push_str(&format!(
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
    ));
}

fn sha256_fingerprint(descriptor: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(descriptor.as_bytes()))
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

pub fn semantic_tokenizer_behavior_fingerprint() -> String {
    semantic_tokenizer_behavior_fingerprint_for(SEMANTIC_REQUIRED_MODEL_FILES)
}

pub(super) fn semantic_tokenizer_behavior_fingerprint_for(files: &[SemanticModelFile]) -> String {
    let mut descriptor = "ctx-semantic-tokenizer-behavior-v1".to_owned();
    for path in SEMANTIC_TOKENIZER_BEHAVIOR_PATHS {
        match files.iter().find(|file| file.path == *path) {
            Some(file) => descriptor.push_str(&format!(
                "|file={}:{}:{}",
                file.path, file.size, file.sha256
            )),
            None => descriptor.push_str(&format!("|missing={path}")),
        }
    }
    sha256_fingerprint(&descriptor)
}

fn semantic_prefixed_text(prefix: &str, text: String) -> String {
    let text = text.trim_start();
    if text.starts_with(prefix) {
        text.to_owned()
    } else {
        format!("{prefix}{text}")
    }
}

pub fn semantic_e5_passage_text(text: &str) -> String {
    builtin_semantic_model_contract().document_text(text)
}

pub(super) fn semantic_e5_query_text(text: &str) -> String {
    builtin_semantic_model_contract().query_text(text)
}
