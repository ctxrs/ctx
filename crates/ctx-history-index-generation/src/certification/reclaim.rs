use super::*;

/// Removes a quiescent candidate while preserving the active generation's
/// certification across its managed hard-link changes. The caller must hold
/// the generation writer lock; `remove` must validate the directory binding.
pub fn reclaim_candidate_with_certifications(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    active_index: &tantivy::Index,
    candidate_directory: &str,
    proof: &crate::CandidatePhysicalProof,
    remove: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let _certification_update = crate::retention::CertificationGuard::update(root)?;
    let slot = pointer.active();
    if matching_certification(root, pointer, slot, active_index)
        .ok()
        .flatten()
        .is_none()
    {
        // Cloning already authenticated these shared files. Rebind the active
        // cache while those exact identities still match, before unlinking the
        // candidate changes their ctimes again. Cache failure keeps the normal
        // full verification on the subsequent reusable-generation open.
        let _ = (|| -> Result<()> {
            let audit = crate::physical_integrity_audit_with_candidate_proof(
                active_index,
                &slot_path(root, slot),
                Some(pointer),
                Some(proof),
            )?;
            if audit.digest() != slot.physical_integrity_digest() {
                return Err(IndexError::ChecksumMismatch);
            }
            install_certification(
                root,
                Some(pointer),
                None,
                slot,
                active_index,
                &audit,
                CertificationInstallPolicy::ACTIVE_CACHE,
            )?;
            Ok(())
        })();
    }
    reclaim_with_pointer_certifications(root, pointer, &[candidate_directory], remove)
}

/// Preserves all authorized retained sidecars across reclamation; failures retain safe hashing.
/// The caller holds the certification update guard and exact directory authorities
/// for every managed deletion target, including the remainder of this batch.
pub(crate) fn reclaim_with_pointer_certifications(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    managed_directories: &[&str],
    remove: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let certifications =
        managed_links::ManagedLinkCertifications::capture(root, pointer, managed_directories).ok();
    remove()?;
    if let Some(certifications) = certifications {
        certifications.finish_reclaim(root);
    }
    Ok(())
}
