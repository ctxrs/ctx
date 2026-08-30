#[cfg(not(windows))]
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
#[cfg(not(windows))]
use ctx_history_platform::platform_security::restrict_private_directory;
use ctx_history_platform::platform_security::restrict_private_file;

pub(crate) fn create_private_dir_all(path: &Path) -> Result<()> {
    #[cfg(windows)]
    ctx_history_platform::platform_security::create_current_user_owned_private_directory_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    #[cfg(not(windows))]
    {
        fs::create_dir_all(path)
            .with_context(|| format!("create private directory {}", path.display()))?;
        restrict_private_directory(path)
            .with_context(|| format!("secure private directory {}", path.display()))?;
    }
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
