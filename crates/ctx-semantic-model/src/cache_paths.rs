use std::path::{Path, PathBuf};

use super::model_contract::SEMANTIC_MODEL_REVISION;

pub(super) const SEMANTIC_HF_MODEL_CACHE_DIR: &str = "models--intfloat--multilingual-e5-small";
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
