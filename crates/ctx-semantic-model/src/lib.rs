mod artifact_fetch;
mod cache_paths;
mod configuration;
mod embedding_executor;
mod health_search;
mod http_embedding_canary;
mod http_embedding_executor;
mod json;
#[cfg(any(target_os = "macos", test, feature = "test-support"))]
#[cfg_attr(
    all(feature = "test-support", not(any(target_os = "macos", test))),
    allow(dead_code)
)]
mod model_acquisition;
#[cfg(any(target_os = "macos", test, feature = "test-support"))]
#[cfg_attr(
    all(feature = "test-support", not(any(target_os = "macos", test))),
    allow(dead_code)
)]
mod model_bundle;
mod model_contract;
mod model_runtime;
mod resource_policy;

pub use artifact_fetch::{ArtifactFetchRequest, ArtifactFetcher};
pub use cache_paths::semantic_managed_model_snapshot_dir;
pub use configuration::{
    SemanticBackendPreference, SemanticCoreMlComputeMode, SemanticModelConfig, SemanticModelPaths,
    SemanticOnnxRuntimePaths,
};
pub use embedding_executor::{
    BuiltinSemanticEmbeddingExecutor, SemanticEmbeddingExecutor, SemanticEmbeddingExecutorConfig,
    SemanticEmbeddingExecutorHandle, SemanticEmbeddingExecutorKind, SemanticEmbeddingExecutorScope,
};
pub use health_search::{
    semantic_model_acquisition_integrity_error, semantic_model_cache_available,
};
pub use http_embedding_executor::{
    semantic_embedding_failure_is_permanent, HttpSemanticEmbeddingExecutor,
    SemanticEmbeddingExecutorAuth, SEMANTIC_EMBEDDING_AUTH_TOKEN_ENDPOINT_ENV,
    SEMANTIC_EMBEDDING_AUTH_TOKEN_ENV,
};
pub use model_contract::{
    semantic_e5_passage_text, semantic_model_contract, semantic_model_contract_descriptor,
    semantic_model_contract_fingerprint, semantic_model_key,
    semantic_provisioning_coreml_asset_matches, semantic_provisioning_model_contract_matches,
    semantic_provisioning_model_path_count, semantic_provisioning_model_path_matches,
    semantic_required_model_file_count, semantic_required_model_file_matches,
    semantic_tokenizer_behavior_fingerprint, semantic_tokenizer_fingerprint, ExternalSemanticSpace,
    PreparedSemanticDocuments, PreparedSemanticQuery, SemanticModelContract,
    SemanticModelLoadDeferred, SemanticOrtModelVariant, BUILTIN_SEMANTIC_EXECUTOR_ROUTE_IDENTITY,
    MAX_EXTERNAL_SEMANTIC_DIMENSIONS, MAX_EXTERNAL_SEMANTIC_SPACE_ID_BYTES, SEMANTIC_BACKEND,
    SEMANTIC_DIMENSIONS, SEMANTIC_LANGUAGE_SCOPE, SEMANTIC_MODEL_CONTRACT_VERSION,
    SEMANTIC_MODEL_ID, SEMANTIC_MODEL_KEY, SEMANTIC_MODEL_REVISION, SEMANTIC_NORMALIZATION,
    SEMANTIC_PASSAGE_PREFIX, SEMANTIC_POOLING, SEMANTIC_QUERY_PREFIX,
};
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use model_runtime::SemanticRuntimeBusyGuard;
pub use model_runtime::{
    prepare_platform_semantic_acceleration, semantic_native_accelerator_target,
    semantic_query_service_supported, SemanticDaemonCpuFallbackRequired,
    SemanticDaemonModelAcquisition, SemanticEmbeddingRuntimeInfo, SemanticNativeAcceleratorTarget,
    SharedSemanticRuntime,
};
pub use resource_policy::{
    semantic_model_load_resource_facts, SemanticModelLoadResourceFacts, SemanticQuietPolicy,
};

#[cfg(all(feature = "test-support", ctx_semantic_fastembed))]
fn write_test_semantic_cache(root: &std::path::Path) -> anyhow::Result<()> {
    let snapshot = root
        .join(cache_paths::SEMANTIC_HF_MODEL_CACHE_DIR)
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION);
    std::fs::create_dir_all(&snapshot)?;
    for file in SemanticOrtModelVariant::CpuFp32.required_files() {
        let path = snapshot.join(file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::File::create(path)?.set_len(file.size)?;
    }
    Ok(())
}

#[cfg(feature = "test-support")]
pub mod test_support {
    use std::path::Path;

    #[doc(hidden)]
    pub fn legacy_fixed_http_query_canary_embedding() -> Vec<f32> {
        super::http_embedding_canary::normalized_query_reference()
    }

    #[doc(hidden)]
    pub fn legacy_fixed_http_document_canary_embedding() -> Vec<f32> {
        super::http_embedding_canary::normalized_document_reference()
    }

    use anyhow::Result;

    use crate::{SemanticDaemonModelAcquisition, SemanticOrtModelVariant};

    /// Returns the normalized frozen HTTP conformance vector for a test server.
    /// Production endpoints must provide their own conforming embeddings.
    pub fn http_embedding_canary_vector(input_kind: &str) -> Vec<f32> {
        let reference = match input_kind {
            "query" => crate::http_embedding_canary::QUERY_DAEMON_RECOVERY_REFERENCE.as_slice(),
            "documents" => {
                crate::http_embedding_canary::DOCUMENT_DAEMON_RECOVERY_REFERENCE.as_slice()
            }
            _ => panic!("unknown HTTP embedding canary input kind: {input_kind}"),
        };
        let norm = reference
            .iter()
            .map(|value| f32::from(*value).powi(2))
            .sum::<f32>()
            .sqrt();
        reference
            .iter()
            .map(|value| f32::from(*value) / norm)
            .collect()
    }

    #[cfg(ctx_semantic_fastembed)]
    pub fn write_test_semantic_cache(root: &Path) -> Result<()> {
        crate::write_test_semantic_cache(root)
    }

    #[cfg(ctx_semantic_fastembed)]
    pub fn load_missing_semantic_onnxruntime(
        model_cache_dir: &Path,
        missing_dylib: &Path,
    ) -> Result<std::path::PathBuf> {
        crate::model_runtime::load_missing_semantic_onnxruntime_for_test(
            model_cache_dir,
            missing_dylib,
        )
    }

    #[cfg(ctx_semantic_fastembed)]
    pub fn map_daemon_coreml_load_error(
        acquisition: SemanticDaemonModelAcquisition,
        error: anyhow::Error,
    ) -> anyhow::Error {
        crate::model_runtime::map_daemon_coreml_load_error(acquisition, error)
    }

    #[cfg(ctx_semantic_fastembed)]
    pub fn write_test_semantic_cache_variant(
        root: &Path,
        variant: SemanticOrtModelVariant,
    ) -> Result<()> {
        let snapshot = root
            .join(crate::cache_paths::SEMANTIC_HF_MODEL_CACHE_DIR)
            .join("snapshots")
            .join(crate::SEMANTIC_MODEL_REVISION);
        std::fs::create_dir_all(&snapshot)?;
        for file in variant.required_files() {
            let path = snapshot.join(file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::File::create(path)?.set_len(file.size)?;
        }
        Ok(())
    }
}

#[cfg(all(test, ctx_semantic_fastembed))]
mod fastembed_policy_tests;
#[cfg(test)]
mod model_contract_tests;
