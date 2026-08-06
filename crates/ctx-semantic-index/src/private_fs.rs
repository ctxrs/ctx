use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ctx_history_core::platform_security::{restrict_private_directory, restrict_private_file};

pub(crate) fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    restrict_private_directory(path)
        .with_context(|| format!("secure private directory {}", path.display()))?;
    Ok(())
}

pub(crate) fn secure_private_file_permissions(path: &Path) -> Result<()> {
    restrict_private_file(path)
        .with_context(|| format!("secure private file {}", path.display()))?;
    Ok(())
}

pub(crate) fn secure_semantic_vector_permissions(path: &Path) -> Result<()> {
    for candidate in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if candidate.exists() {
            restrict_private_file(&candidate)
                .with_context(|| format!("secure semantic vector file {}", candidate.display()))?;
        }
    }
    Ok(())
}
