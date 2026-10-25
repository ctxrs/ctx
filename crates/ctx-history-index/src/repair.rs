use std::path::{Path, PathBuf};

use ctx_history_index_generation::{
    acquire_generation_writer_lock_with_retry,
    ensure_generation_control_files_private_with_writer_lock_held,
    quarantine_invalid_retention_lease_with_writer_lock_held, DurableMmapDirectory,
    GENERATION_WRITER_LOCK_FILE,
};
use serde::Serialize;
use tantivy::directory::Lock;

use crate::{Result, VerifiedIndex};

#[derive(Debug, Serialize)]
pub struct GenerationLeaseRepair {
    pub generation_id: String,
    pub quarantined_lease: Option<PathBuf>,
}

/// Repairs coordination state without importing, publishing, or reclaiming
/// history. A proven-invalid current-version hold is quarantined only after
/// the active Core generation verifies under the writer lock. Live OS reader
/// locks and valid, unsupported, oversized, or unclassifiable holds remain.
pub fn repair_generation_leases(root: &Path) -> Result<GenerationLeaseRepair> {
    // Do not turn a missing index into an initialized empty one.
    std::fs::symlink_metadata(root)?;
    ctx_history_platform::platform_security::ensure_private_directory(root)?;
    let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let root = directory.root_path();
    let lock = Lock {
        filepath: GENERATION_WRITER_LOCK_FILE.into(),
        is_blocking: false,
    };
    let _guard = acquire_generation_writer_lock_with_retry(&directory, &lock)?;
    if ensure_generation_control_files_private_with_writer_lock_held(root)? {
        ctx_history_index_format::clear_manifest_cache_for_root(root)?;
    }
    let verified = VerifiedIndex::open_pinned(root)?;
    let quarantined_lease = quarantine_invalid_retention_lease_with_writer_lock_held(root)?;
    Ok(GenerationLeaseRepair {
        generation_id: verified.generation_id().to_owned(),
        quarantined_lease,
    })
}
