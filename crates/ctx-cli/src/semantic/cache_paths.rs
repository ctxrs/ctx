use std::path::{Path, PathBuf};

use super::model_contract::{SEMANTIC_HF_MODEL_CACHE_DIR, SEMANTIC_MANAGED_MODEL_CACHE_DIR};

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
