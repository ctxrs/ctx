use std::path::{Path, PathBuf};

pub(crate) fn bundle_manifest_path(bundle_dir: &Path) -> PathBuf {
    if let Ok(raw) = std::env::var("CTX_BUNDLE_MANIFEST") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let candidate = PathBuf::from(trimmed);
            if candidate.is_absolute() {
                return candidate;
            }
            return bundle_dir.join(candidate);
        }
    }
    let effective_manifest = bundle_dir.join("runtime_manifest.effective.json");
    if effective_manifest.exists() {
        return effective_manifest;
    }
    bundle_dir.join("manifest.json")
}

pub(crate) fn bundled_artifact_identity_path(bundle_dir: &Path) -> PathBuf {
    bundle_dir.join("artifact_identity.json")
}

pub(crate) fn bundled_provider_manifest_path(bundle_dir: &Path) -> PathBuf {
    bundle_dir.join("provider_matrix.json")
}
