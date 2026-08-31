use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
#[cfg(not(windows))]
use ctx_history_platform::platform_security::ensure_private_directory;
use ctx_history_platform::platform_security::restrict_private_file;

/// Resolves only the existing parent prefix used by ordinary writable opens.
/// Missing components and the final semantic root remain unresolved so the
/// private directory walker can create and validate them without following.
pub(crate) fn writable_private_root(path: &Path) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("semantic root has no final path component"))?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = match std::fs::canonicalize(parent) {
            Ok(parent) => parent,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                writable_private_root(parent)?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("resolve semantic root parent {}", parent.display()))
            }
        };
        Ok(parent.join(name))
    }
    #[cfg(not(unix))]
    {
        Ok(path.to_path_buf())
    }
}

pub(crate) fn create_private_dir_all(path: &Path) -> Result<()> {
    #[cfg(windows)]
    ctx_history_platform::platform_security::create_current_user_owned_private_directory_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    #[cfg(not(windows))]
    ensure_private_directory(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
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
