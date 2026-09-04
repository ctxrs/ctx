use super::*;

/// Preserves pointer-retained sidecars across reclamation; failures retain safe hashing.
pub(crate) fn reclaim_with_pointer_certifications(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    reclaimed_directory: &str,
    remove: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let certifications = [Some(pointer.active()), pointer.previous()]
        .into_iter()
        .flatten()
        .filter_map(|slot| {
            prepare_reclaim(root, pointer, slot, reclaimed_directory)
                .ok()
                .flatten()
        })
        .collect::<Vec<_>>();
    remove()?;
    for mut certification in certifications {
        let _ = (|| {
            let slot = certification.slot.clone();
            let generation_path = slot_path(root, &slot);
            for expected in &mut certification.artifacts {
                let current = capture_artifact(
                    root,
                    &generation_path,
                    Path::new(&expected.artifact.path),
                    Some(pointer),
                )?;
                if current != expected.artifact
                    && (!current
                        .identity
                        .same_payload_identity(&expected.artifact.identity)
                        || current.identity.link_count().checked_add(1)
                            != Some(expected.artifact.identity.link_count()))
                {
                    return Err(IndexError::ChecksumMismatch);
                }
                expected.artifact = current;
            }
            let index = crate::open_slot_index(root, &slot)?;
            install_certification_sidecar(
                root,
                Some(pointer),
                None,
                &slot,
                &index,
                &certification,
                false,
            )
        })();
    }
    Ok(())
}

fn prepare_reclaim(
    root: &Path,
    pointer: &ActiveGenerationPointer,
    slot: &GenerationSlot,
    reclaimed_directory: &str,
) -> Result<Option<GenerationIntegrityCertification>> {
    let Some(certification) = load_structurally_valid_certification(root, slot)? else {
        return Ok(None);
    };
    let aliases = std::iter::once(pointer.active().directory())
        .chain(pointer.previous().map(GenerationSlot::directory))
        .chain(std::iter::once(reclaimed_directory))
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let generation_path = slot_path(root, slot);
    for expected in &certification.artifacts {
        if capture_artifact_with_retained_aliases(
            root,
            &generation_path,
            Path::new(&expected.artifact.path),
            &aliases,
        )? != expected.artifact
        {
            return Ok(None);
        }
    }
    Ok(Some(certification))
}
