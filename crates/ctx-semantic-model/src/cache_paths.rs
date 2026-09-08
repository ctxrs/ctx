use std::path::{Path, PathBuf};

use super::model_contract::{SemanticOrtModelVariant, SEMANTIC_MODEL_REVISION};

pub(super) const SEMANTIC_HF_MODEL_CACHE_DIR: &str = "models--intfloat--multilingual-e5-small";
/// The accelerator graph is stored at the same contract path as the fp32 graph
/// but holds different bytes, so the two variants cannot share a model root:
/// publishing one would evict the other and make a CPU fallback and an
/// accelerator install re-download each other forever. The fp32 root keeps its
/// historical name so existing installs are found without migration.
pub(super) const SEMANTIC_ACCELERATOR_MODEL_CACHE_DIR: &str =
    "models--intfloat--multilingual-e5-small--o4-fp16";
pub(super) const SEMANTIC_MANAGED_MODEL_CACHE_DIR: &str = "ctx-semantic-models";

#[allow(dead_code)] // Signed provisioning consumes this seam in a separate integration lane.
pub fn semantic_managed_model_snapshot_dir(cache_dir: &Path) -> PathBuf {
    cache_dir
        .join(SEMANTIC_MANAGED_MODEL_CACHE_DIR)
        .join(SEMANTIC_HF_MODEL_CACHE_DIR)
        .join("snapshots")
        .join(SEMANTIC_MODEL_REVISION)
}

pub(super) fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

pub(super) fn semantic_model_cache_roots(cache_dir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    push_unique_path(
        &mut roots,
        cache_dir
            .join(SEMANTIC_MANAGED_MODEL_CACHE_DIR)
            .join(SEMANTIC_HF_MODEL_CACHE_DIR),
    );
    if cache_dir.file_name().and_then(|name| name.to_str()) == Some(SEMANTIC_HF_MODEL_CACHE_DIR) {
        push_unique_path(&mut roots, cache_dir.to_path_buf());
    }
    push_unique_path(&mut roots, cache_dir.join(SEMANTIC_HF_MODEL_CACHE_DIR));
    push_unique_path(
        &mut roots,
        cache_dir.join("hub").join(SEMANTIC_HF_MODEL_CACHE_DIR),
    );
    roots
}

/// Model cache roots to search for `variant`, most authoritative first.
///
/// Every reader (loader, health/status reporting, provisioning) must resolve
/// through this so status can never claim a cache that belongs to the other
/// variant. Accelerator lookups also scan the shared fp32 roots because a
/// snapshot placed there is still verified against the accelerator pins before
/// use; only publication is variant-exclusive.
pub(super) fn semantic_ort_model_cache_roots(
    cache_dir: &Path,
    variant: SemanticOrtModelVariant,
) -> Vec<PathBuf> {
    match variant {
        SemanticOrtModelVariant::CpuFp32 => semantic_model_cache_roots(cache_dir),
        SemanticOrtModelVariant::AcceleratorO4Fp16 => {
            let mut roots = vec![semantic_ort_published_model_root(cache_dir, variant)];
            for root in semantic_model_cache_roots(cache_dir) {
                push_unique_path(&mut roots, root);
            }
            roots
        }
    }
}

/// The single root that provisioning publishes `variant` into.
pub(super) fn semantic_ort_published_model_root(
    cache_dir: &Path,
    variant: SemanticOrtModelVariant,
) -> PathBuf {
    cache_dir
        .join(SEMANTIC_MANAGED_MODEL_CACHE_DIR)
        .join(semantic_ort_model_cache_dir(variant))
}

pub(super) fn semantic_ort_model_cache_dir(variant: SemanticOrtModelVariant) -> &'static str {
    match variant {
        SemanticOrtModelVariant::CpuFp32 => SEMANTIC_HF_MODEL_CACHE_DIR,
        SemanticOrtModelVariant::AcceleratorO4Fp16 => SEMANTIC_ACCELERATOR_MODEL_CACHE_DIR,
    }
}
