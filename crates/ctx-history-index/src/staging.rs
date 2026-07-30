use super::*;

pub(super) struct PendingSource {
    pub(super) source: SourceKey,
    pub(super) mode: PendingSourceMode,
    pub(super) staged_documents: u64,
    pub(super) certificate: Option<CertifiedSource>,
}

// Keep the append base inline to avoid allocation and indirection.
#[allow(clippy::large_enum_variant)]
pub(super) enum PendingSourceMode {
    Replace,
    Append { base: CertifiedSource },
}

/// Discards a completed one-pass staging run when it reproduced the full
/// verified base manifest exactly.
///
/// Requiring every retained base source to have a pending certificate prevents
/// an incomplete route scan from turning a carried source into a false no-op.
/// The already-created Tantivy writer is rolled back, so physical churn that
/// leaves the logical corpus unchanged does not publish `meta.json`, advance
/// the opstamp, or create a generation.
pub(super) fn finish_identical_staging<F, I>(
    generation: &mut GenerationWriter,
    manifest: &GenerationManifest,
    revalidate: &mut F,
    revalidate_inventory: &mut I,
) -> Result<Option<CommitReceipt>>
where
    F: FnMut(RevalidationTarget<'_>) -> bool,
    I: FnMut(&CertifiedSourceInventory) -> bool,
{
    if !staged_manifest_matches_base(generation, manifest)? {
        return Ok(None);
    }

    for pending in generation.pending.values() {
        let certificate = pending
            .certificate
            .as_ref()
            .ok_or_else(|| IndexError::SourceNotCertified(pending.source.identity().to_string()))?;
        if !revalidate(RevalidationTarget::Source(certificate)) {
            return Err(IndexError::SourceInvalidated(
                pending.source.identity().to_string(),
            ));
        }
    }
    for removal in generation.deletions.values() {
        if !revalidate(RevalidationTarget::Deletion(removal.deletion())) {
            return Err(IndexError::SourceInvalidated(
                removal.source().identity().to_string(),
            ));
        }
    }
    for inventory in &generation.complete_inventories {
        if !revalidate_inventory(inventory) {
            return Err(IndexError::CompleteInventoryInvalidated {
                provider: inventory.observation().provider().to_owned(),
                authority_namespace: inventory.observation().authority_namespace().to_owned(),
            });
        }
    }

    let base = generation
        .base_manifest
        .clone()
        .ok_or(IndexError::WriterInvariant(
            "staged no-op is missing its verified base manifest",
        ))?;
    let mut writer = generation.writer.take().ok_or(IndexError::WriterInvariant(
        "staged no-op is missing its Tantivy writer",
    ))?;
    writer.rollback()?;
    writer.wait_merging_threads()?;
    CommitReceipt::from_manifest(generation.base_opstamp, base).map(Some)
}

fn staged_manifest_matches_base(
    generation: &GenerationWriter,
    manifest: &GenerationManifest,
) -> Result<bool> {
    if generation.writer.is_none() {
        return Ok(false);
    }
    let Some(base) = generation.base_manifest.as_ref() else {
        return Ok(false);
    };
    if generation.pending.len() != base.sources.len()
        || base.sources.iter().any(|source| {
            !generation
                .pending
                .contains_key(&source_token(source.observation().source()))
        })
    {
        return Ok(false);
    }
    Ok(manifest.generation_id()? == base.generation_id()?)
}
