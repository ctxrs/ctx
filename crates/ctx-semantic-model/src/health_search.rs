#[cfg(ctx_semantic_fastembed)]
#[derive(Debug, Clone)]
pub(super) struct SemanticEmbedPolicy {
    pub(super) threads: usize,
    pub(super) batch_size: usize,
    pub(super) available_memory_bytes: Option<u64>,
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embed_policy_for(
    compute_class: SemanticComputeClass,
    config: &crate::SemanticModelConfig,
) -> SemanticEmbedPolicy {
    semantic_embed_policy_from_config_and_resources(
        compute_class,
        config,
        SemanticSystemResources::current(),
    )
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embed_policy_from_config_and_resources(
    compute_class: SemanticComputeClass,
    config: &crate::SemanticModelConfig,
    resources: SemanticSystemResources,
) -> SemanticEmbedPolicy {
    let quiet = semantic_quiet_policy(resources, compute_class);
    let mut policy = SemanticEmbedPolicy {
        threads: quiet.threads,
        batch_size: quiet.batch_size,
        available_memory_bytes: resources.available_memory_bytes,
    };
    if let Some(threads) = config.thread_override() {
        policy.threads = threads.min(SEMANTIC_EMBED_THREADS_MAX);
    }
    if let Some(batch_size) = config.batch_size_override() {
        policy.batch_size = batch_size.min(SEMANTIC_EMBED_BATCH_MAX);
    }
    policy
}

pub fn semantic_model_cache_available(cache_dir: &Path) -> bool {
    semantic_model_cache_snapshot_dir(cache_dir).is_some()
        || semantic_accelerator_model_cache_available(cache_dir)
        || semantic_coreml_model_cache_available(cache_dir)
}

pub(super) fn semantic_accelerator_model_cache_available(cache_dir: &Path) -> bool {
    semantic_ort_model_cache_snapshot_dir(cache_dir, SemanticOrtModelVariant::AcceleratorO4Fp16)
        .is_some()
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn semantic_coreml_model_cache_available(cache_dir: &Path) -> bool {
    coreml_bundle_cache_available(cache_dir)
}

#[cfg(not(any(target_os = "macos", test)))]
pub(super) fn semantic_coreml_model_cache_available(_cache_dir: &Path) -> bool {
    false
}

pub fn semantic_model_acquisition_integrity_error(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<SemanticCpuModelIntegrityError>()
        .is_some()
    {
        return true;
    }
    #[cfg(any(target_os = "macos", test))]
    {
        model_acquisition_integrity_error(error)
    }
    #[cfg(not(any(target_os = "macos", test)))]
    false
}

pub(super) fn semantic_model_cache_snapshot_dir(cache_dir: &Path) -> Option<PathBuf> {
    semantic_ort_model_cache_snapshot_dir(cache_dir, SemanticOrtModelVariant::CpuFp32)
}

fn semantic_ort_model_cache_snapshot_dir(
    cache_dir: &Path,
    variant: SemanticOrtModelVariant,
) -> Option<PathBuf> {
    if !semantic_embedding_supported() {
        return None;
    }
    if cache_dir.as_os_str().is_empty() {
        return None;
    }
    for model_root in cache_paths::semantic_model_cache_roots(cache_dir) {
        if let Some(snapshot) = semantic_ort_model_snapshot_from_root(&model_root, variant) {
            return Some(snapshot);
        }
    }
    None
}

fn semantic_ort_model_snapshot_from_root(
    model_root: &Path,
    variant: SemanticOrtModelVariant,
) -> Option<PathBuf> {
    let snapshot = model_root.join("snapshots").join(SEMANTIC_MODEL_REVISION);
    if !snapshot.is_dir() {
        return None;
    }
    if variant.required_files().all(|file| {
        fs::metadata(snapshot.join(file.path))
            .map(|metadata| metadata.is_file() && metadata.len() == file.size)
            .unwrap_or(false)
    }) {
        Some(snapshot)
    } else {
        None
    }
}

#[cfg(ctx_semantic_fastembed)]
pub(super) fn semantic_embedding_supported() -> bool {
    true
}

#[cfg(not(ctx_semantic_fastembed))]
pub(super) fn semantic_embedding_supported() -> bool {
    false
}

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    cache_paths,
    model_contract::{
        SemanticCpuModelIntegrityError, SemanticOrtModelVariant, SEMANTIC_MODEL_REVISION,
    },
    resource_policy::{
        semantic_quiet_policy, SemanticComputeClass, SemanticSystemResources,
        SEMANTIC_EMBED_BATCH_MAX, SEMANTIC_EMBED_THREADS_MAX,
    },
};

#[cfg(any(target_os = "macos", test))]
use crate::model_acquisition::{coreml_bundle_cache_available, model_acquisition_integrity_error};
