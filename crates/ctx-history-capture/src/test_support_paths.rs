use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) fn tempdir() -> io::Result<tempfile::TempDir> {
    let temp_root = fs::canonicalize(std::env::temp_dir())?;
    tempfile::Builder::new()
        .prefix("ctx-history-capture-")
        .tempdir_in(temp_root)
}

pub(crate) fn capture_manifest_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.is_absolute() {
        return manifest;
    }

    if let Ok(current_dir) = std::env::current_dir() {
        if let Some(path) = manifest_dir_from(&current_dir, &manifest) {
            return path;
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        for ancestor in current_exe.ancestors() {
            if let Some(path) = manifest_dir_from(ancestor, &manifest) {
                return path;
            }
        }
    }

    manifest
}

pub(crate) fn capture_repo_root() -> PathBuf {
    capture_manifest_dir()
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn manifest_dir_from(base: &Path, manifest: &Path) -> Option<PathBuf> {
    let candidate = base.join(manifest);
    if candidate.join("Cargo.toml").is_file() {
        return fs::canonicalize(&candidate).ok().or(Some(candidate));
    }
    None
}
