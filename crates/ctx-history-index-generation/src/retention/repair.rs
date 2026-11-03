use super::*;

/// Quarantines a proven-invalid current-version hold after the caller verifies the active
/// generation while holding the publication writer lock. Valid holds, including
/// unrelated consumers, remain intact, as do oversized or unclassifiable holds.
/// No generation data is reclaimed.
#[doc(hidden)]
pub fn quarantine_invalid_retention_lease_with_writer_lock_held(
    root: &Path,
) -> Result<Option<PathBuf>> {
    let path = lease_path(root);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            ctx_history_platform::platform_security::ensure_private_file(&path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(_) => return Err(IndexError::InvalidGenerationRetentionLease),
        Err(error) => return Err(error.into()),
    }
    match load_generation_retention_lease(root) {
        Ok(_) => Ok(None),
        Err(
            IndexError::InvalidGenerationRetentionLease
            | IndexError::InvalidGenerationRetentionLeaseOwner,
        ) => {
            let quarantine = root.join(format!(
                ".generation-retention-lease.invalid-{}",
                uuid::Uuid::now_v7().simple()
            ));
            fs::rename(&path, &quarantine)?;
            sync_directory(root)?;
            Ok(Some(quarantine))
        }
        Err(error) => Err(error),
    }
}
