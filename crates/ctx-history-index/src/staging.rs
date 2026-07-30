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
    Retain { base: CertifiedSource },
}

impl GenerationWriter {
    /// Retains one source after a full logical rescan reproduced its exact
    /// certified base. This records current-source coverage without opening
    /// Tantivy or requiring an append frontier.
    pub fn retain_source(&mut self, certificate: CertifiedSource) -> Result<()> {
        certificate.validate_contract()?;
        let source = certificate.observation().source();
        register_compact_identity(
            &mut self.source_identities,
            source.identity(),
            "source",
            false,
        )?;
        let token = source_token(source);
        if self.pending.contains_key(&token) {
            return Err(IndexError::DuplicateSource(source.identity().to_string()));
        }
        let base =
            self.base_manifest
                .as_ref()
                .and_then(|manifest| {
                    manifest.sources.iter().find(|candidate| {
                        candidate.observation().source().exact_descriptor_eq(source)
                    })
                })
                .cloned()
                .ok_or_else(|| IndexError::SourceNotAppendable(source.identity().to_string()))?;
        if base != certificate {
            return Err(IndexError::SourceCertificateMismatch);
        }
        self.deletions.remove(source);
        self.pending.insert(
            token.clone(),
            super::PendingSource {
                index_fields: IndexSourceFields::new(source, &token),
                staged: PendingSource {
                    source: source.clone(),
                    mode: PendingSourceMode::Retain { base },
                    staged_documents: 0,
                    certificate: Some(certificate),
                },
            },
        );
        Ok(())
    }
}

pub(super) fn verify_published_mutations(
    generation: &GenerationWriter,
    verified: &VerifiedIndex,
) -> Result<()> {
    for pending in generation.pending.values() {
        let certificate = pending
            .certificate
            .as_ref()
            .ok_or_else(|| IndexError::SourceNotCertified(pending.source.identity().to_string()))?;
        let changed = match &pending.mode {
            PendingSourceMode::Replace => true,
            PendingSourceMode::Append { base } | PendingSourceMode::Retain { base } => {
                certificate != base
            }
        };
        if changed {
            crate::publication::verify_source_document_count(&verified.searcher, certificate)?;
        }
    }
    for removal in generation.deletions.values() {
        crate::publication::verify_source_absent(&verified.searcher, removal.source())?;
    }
    Ok(())
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
