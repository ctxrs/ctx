use super::*;

/// Verifies one immutable generation from its existing publication-time
/// certification without changing durable state.
///
/// The certification remains bound to the exact slot, manifest file, artifact
/// path set, and exact native files after the active pointer moves on. Any
/// metadata transition invalidates the inherited SHA authority because a
/// later link/unlink can mask an intervening same-size, restored-mtime write.
/// Missing, malformed, or otherwise unsupported certification fails closed.
/// Active candidate-link changes and previous-slot publication changes are
/// checked against the full digest; other stale certifications fail closed.
pub fn verify_physical_integrity_read_only(
    root: &Path,
    slot: &GenerationSlot,
    index: &tantivy::Index,
) -> Result<()> {
    // Keep the retained-alias validation on the fast path. The publication
    // matcher alone would also accept aliases in unretained directories.
    // An unchanged certificate must not wait for unrelated bulk work or
    // initialize a coordinator merely to verify a cold immutable snapshot.
    if matches!(
        verify_read_only_snapshot(root, slot, index, false),
        Ok(true)
    ) {
        return Ok(());
    }
    let _certification_guard = crate::retention::CertificationGuard::read_existing(root)?;
    verify_read_only_snapshot(root, slot, index, true).map(|_| ())
}

fn verify_read_only_snapshot(
    root: &Path,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    allow_audit: bool,
) -> Result<bool> {
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    let generation_path = slot_path(root, slot);
    ensure_real_directory(&generation_path)?;
    if crate::read_root::has_retained_read_authority(root, slot.generation_id()) {
        if !allow_audit {
            return Ok(false);
        }
        return crate::verify_physical_integrity(
            index,
            &generation_path,
            None,
            slot.physical_integrity_digest(),
        )
        .map(|()| true);
    }
    ensure_real_directory(&root.join(CERTIFICATION_DIRECTORY))?;

    let bytes =
        read_certification(&certification_path(root, slot)).ok_or(IndexError::ChecksumMismatch)?;
    let certification = serde_json::from_slice::<GenerationIntegrityCertification>(&bytes)
        .map_err(|_| IndexError::ChecksumMismatch)?;
    if serde_json::to_vec(&certification)? != bytes
        || certification.version != CERTIFICATION_VERSION
        || certification.slot != *slot
        || !certification_digest_matches_slot(&certification)?
        || capture_single_link_control(&manifest_path(root, slot.generation_id()))?
            != certification.manifest_identity
    {
        return Err(IndexError::ChecksumMismatch);
    }

    let expected_paths = expected_artifact_paths(index)?;
    if certification
        .artifacts
        .iter()
        .map(|artifact| artifact.artifact.path.clone())
        .collect::<Vec<_>>()
        != expected_paths
    {
        return Err(IndexError::ChecksumMismatch);
    }
    let current_pointer = load_current_pointer(root)?;
    let pointer_fence = ActiveGenerationPointerFence::capture(root, Some(&current_pointer))?;
    let alias_authority = CertificationAliasAuthority::capture(root, &pointer_fence, slot)?;
    for expected in &certification.artifacts {
        let current = capture_artifact_with_retained_aliases(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            alias_authority.directories(),
        )?;
        if current != expected.artifact {
            if !allow_audit {
                return Ok(false);
            }
            if current_pointer.active() == slot {
                if !expected.artifact.same_payload_identity_changed(&current) {
                    return Err(IndexError::ChecksumMismatch);
                }
                // Candidate links change metadata; rehash against the pointer digest.
                crate::verify_physical_integrity(
                    index,
                    &generation_path,
                    Some(&current_pointer),
                    slot.physical_integrity_digest(),
                )?;
            } else {
                verify_certified_previous_after_publication(
                    root,
                    slot,
                    index,
                    &generation_path,
                    &current_pointer,
                )?;
            }
            alias_authority.validate(root, &pointer_fence)?;
            return Ok(true);
        }
    }
    alias_authority.validate(root, &pointer_fence)?;
    Ok(true)
}

fn verify_certified_previous_after_publication(
    root: &Path,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    generation_path: &Path,
    pointer: &ActiveGenerationPointer,
) -> Result<()> {
    if pointer.previous() != Some(slot) {
        return Err(IndexError::ChecksumMismatch);
    }
    let active_index =
        crate::open_slot_index(root, pointer.active()).map_err(|_| IndexError::ChecksumMismatch)?;
    verify_physical_integrity_read_only(root, pointer.active(), &active_index).map_err(
        |error| {
            if matches!(error, IndexError::ConcurrentGenerationChange) {
                error
            } else {
                IndexError::ChecksumMismatch
            }
        },
    )?;
    crate::verify_physical_integrity(
        index,
        generation_path,
        Some(pointer),
        slot.physical_integrity_digest(),
    )?;
    if load_current_pointer(root)? != *pointer {
        return Err(IndexError::ConcurrentGenerationChange);
    }
    Ok(())
}
