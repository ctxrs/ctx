#[cfg(ctx_semantic_fastembed)]
pub(in crate::semantic) fn acquire_cpu_backend(
    cache_dir: &Path,
    policy: SemanticEmbedPolicy,
    preference: BackendPreference,
    allow_download: bool,
) -> Result<SemanticEmbedder> {
    if let Some(deferred) = semantic_cpu_model_load_deferred(policy.available_memory_bytes) {
        return Err(deferred.into());
    }
    if env::var_os("CTX_SEMANTIC_MODEL_ONNX").is_some() {
        return Err(anyhow!(
            "CTX_SEMANTIC_MODEL_ONNX is no longer accepted; CPU embeddings use the verified {SEMANTIC_MODEL_ID} cache"
        ));
    }
    let (snapshot, downloaded) = match semantic_cpu_cache_snapshot(cache_dir) {
        Ok(snapshot) => {
            maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition(
                cache_dir,
                allow_download,
            );
            (snapshot, false)
        }
        Err(error) if allow_download && semantic_cpu_cache_repairable(&error) => (
            replace_cpu_model_cache_from_pinned_revision(cache_dir)?,
            true,
        ),
        Err(error) => return Err(error),
    };
    let model = load_cached_cpu_model(&snapshot, cache_dir, &policy)?;
    Ok(SemanticEmbedder {
        backend: SemanticEmbeddingBackend::Cpu(model),
        batch_size: policy.batch_size,
        policy,
        preference,
        acquisition_source: if downloaded { "download" } else { "cache" },
        acquisition_fallback: None,
    })
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn load_cached_cpu_model(
    snapshot: &Path,
    cache_dir: &Path,
    policy: &SemanticEmbedPolicy,
) -> Result<fastembed::TextEmbedding> {
    use fastembed::{
        EmbeddingModel, InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles,
        UserDefinedEmbeddingModel,
    };

    let _runtime = ensure_semantic_onnxruntime_loaded(cache_dir)?;
    let model_info = TextEmbedding::get_model_info(&EmbeddingModel::MultilingualE5Small)?;
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: read_semantic_model_file(snapshot, "tokenizer.json")?,
        config_file: read_semantic_model_file(snapshot, "config.json")?,
        special_tokens_map_file: read_semantic_model_file(snapshot, "special_tokens_map.json")?,
        tokenizer_config_file: read_semantic_model_file(snapshot, "tokenizer_config.json")?,
    };
    let model_path = snapshot.join(&model_info.model_file);
    let mut user_model = UserDefinedEmbeddingModel::new(
        fs::read(&model_path)
            .with_context(|| format!("read semantic model file {}", model_path.display()))?,
        tokenizer_files,
    )
    .with_pooling(
        TextEmbedding::get_default_pooling_method(&EmbeddingModel::MultilingualE5Small)
            .unwrap_or(Pooling::Mean),
    )
    .with_quantization(TextEmbedding::get_quantization_mode(
        &EmbeddingModel::MultilingualE5Small,
    ));
    user_model.output_key = model_info.output_key.clone();
    TextEmbedding::try_new_from_user_defined(
        user_model,
        InitOptionsUserDefined::new().with_intra_threads(policy.threads),
    )
    .with_context(|| format!("initialize semantic embedding model {SEMANTIC_MODEL_ID}"))
}
use std::{env, fs, path::Path};

use anyhow::{anyhow, Context, Result};

use super::{
    maybe_cleanup_semantic_cpu_download_cache_after_cached_acquisition,
    onnx::ensure_semantic_onnxruntime_loaded, read_semantic_model_file,
    replace_cpu_model_cache_from_pinned_revision, semantic_cpu_cache_repairable,
    semantic_cpu_cache_snapshot, BackendPreference, SemanticEmbedder, SemanticEmbeddingBackend,
};
use crate::semantic::{
    health_search::SemanticEmbedPolicy, model_contract::SEMANTIC_MODEL_ID,
    resource_policy::semantic_cpu_model_load_deferred,
};
