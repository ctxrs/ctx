use super::*;
use std::collections::HashMap;

#[derive(Clone)]
pub(super) struct PendingSource {
    pub(super) source: SourceKey,
    pub(super) mode: PendingSourceMode,
    pub(super) staged_documents: u64,
    pub(super) certificate: Option<CertifiedSource>,
    pub(super) core_record_accumulator: [u8; 32],
}

// Keep the append base inline to avoid allocation and indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(super) enum PendingSourceMode {
    Replace,
    Append { base: CertifiedSource },
    Retain { base: CertifiedSource },
}

impl GenerationWriter {
    /// Retains one source's indexed Core records after a full logical rescan
    /// reproduced them exactly.
    ///
    /// The current certificate may advance only its replay frontier. This
    /// records current-source coverage without restaging documents while
    /// still letting a durable physical-change hint move forward.
    pub fn retain_source(&mut self, certificate: CertifiedSource) -> Result<()> {
        certificate.validate_contract()?;
        let source = certificate.observation().source();
        self.reject_carried_source_mutation(source)?;
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
            self.base_publication
                .as_ref()
                .map(PinnedPublication::manifest)
                .and_then(|manifest| {
                    manifest.sources.iter().find(|candidate| {
                        candidate.observation().source().exact_descriptor_eq(source)
                    })
                })
                .cloned()
                .ok_or_else(|| IndexError::SourceNotAppendable(source.identity().to_string()))?;
        if !retained_core_records_match(&base, &certificate) {
            return Err(IndexError::SourceCertificateMismatch);
        }
        self.deletions.remove(source);
        self.route_deletions.remove(source);
        self.pending.insert(
            token.clone(),
            super::PendingSource {
                staged: PendingSource {
                    source: source.clone(),
                    mode: PendingSourceMode::Retain { base },
                    staged_documents: 0,
                    certificate: Some(certificate),
                    core_record_accumulator: [0; 32],
                },
            },
        );
        Ok(())
    }
}

fn retained_core_records_match(base: &CertifiedSource, current: &CertifiedSource) -> bool {
    base.observation() == current.observation()
        && base.parser_revision() == current.parser_revision()
        && base.content_digest() == current.content_digest()
        && base.counts() == current.counts()
}

fn source_record_aggregate(
    source: &SourceKey,
    indexed_documents: u64,
    core_record_accumulator: [u8; 32],
) -> Result<SourceCoreRecordAggregate> {
    SourceCoreRecordAggregate::new(
        source_token(source),
        indexed_documents,
        hex(&core_record_accumulator),
    )
}

pub(super) fn manifest_record_aggregates(
    generation: &GenerationWriter,
    sources: &[CertifiedSource],
) -> Result<Vec<SourceCoreRecordAggregate>> {
    let mut base_aggregates = generation
        .base_publication
        .as_ref()
        .map(PinnedPublication::manifest)
        .map(|manifest| {
            manifest
                .core_record_aggregates
                .iter()
                .cloned()
                .map(|aggregate| (aggregate.source_identity_digest().to_owned(), aggregate))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    for removal in generation.deletions.values() {
        base_aggregates.remove(&source_token(removal.source()));
    }
    for source in &generation.route_deletions {
        base_aggregates.remove(&source_token(source));
    }

    for pending in generation.pending.values() {
        let certificate = pending
            .certificate
            .as_ref()
            .ok_or_else(|| IndexError::SourceNotCertified(pending.source.identity().to_string()))?;
        let aggregate = match &pending.mode {
            PendingSourceMode::Replace => source_record_aggregate(
                &pending.source,
                pending.staged_documents,
                pending.core_record_accumulator,
            )?,
            PendingSourceMode::Retain { .. } => base_aggregates
                .get(&source_token(&pending.source))
                .cloned()
                .ok_or(IndexError::WriterInvariant(
                    "retained source is missing its base Core-record aggregate",
                ))?,
            PendingSourceMode::Append { base } => {
                let base_aggregate = base_aggregates
                    .get(&source_token(&pending.source))
                    .cloned()
                    .ok_or(IndexError::WriterInvariant(
                        "append source is missing its base Core-record aggregate",
                    ))?;
                if base_aggregate.indexed_documents() != base.counts().indexed_documents {
                    return Err(IndexError::CoreRecordAggregateCountMismatch {
                        source_id: source_token(&pending.source),
                        manifest: base.counts().indexed_documents,
                        index: base_aggregate.indexed_documents(),
                    });
                }
                let indexed_documents = base_aggregate
                    .indexed_documents()
                    .checked_add(pending.staged_documents)
                    .ok_or(IndexError::CountOverflow)?;
                let mut accumulator = base_aggregate.accumulator_bytes()?;
                accumulate_core_record(&mut accumulator, &pending.core_record_accumulator);
                source_record_aggregate(&pending.source, indexed_documents, accumulator)?
            }
        };
        if aggregate.indexed_documents() != certificate.counts().indexed_documents {
            return Err(IndexError::CoreRecordAggregateCountMismatch {
                source_id: source_token(&pending.source),
                manifest: certificate.counts().indexed_documents,
                index: aggregate.indexed_documents(),
            });
        }
        base_aggregates.insert(source_token(&pending.source), aggregate);
    }

    let aggregates = sources
        .iter()
        .map(|source| {
            base_aggregates
                .remove(&source_token(source.observation().source()))
                .ok_or(IndexError::WriterInvariant(
                    "published source is missing its Core-record aggregate",
                ))
        })
        .collect::<Result<Vec<_>>>()?;
    if !base_aggregates.is_empty() {
        return Err(IndexError::WriterInvariant(
            "Core-record aggregate exists for an unpublished source",
        ));
    }
    Ok(aggregates)
}

pub(super) fn manifest_source_replacements(
    generation: &GenerationWriter,
) -> Result<Vec<(CertifiedSource, SourceCoreRecordAggregate)>> {
    let base = generation
        .base_publication
        .as_ref()
        .map(PinnedPublication::manifest)
        .ok_or(IndexError::WriterInvariant(
            "source replacement fast path is missing its base manifest",
        ))?;
    let mut replacements = Vec::with_capacity(generation.pending.len());
    for pending in generation.pending.values() {
        let certificate = pending
            .certificate
            .as_ref()
            .ok_or_else(|| IndexError::SourceNotCertified(pending.source.identity().to_string()))?;
        let source_id = source_token(&pending.source);
        let base_aggregate = base
            .core_record_aggregates
            .binary_search_by(|aggregate| aggregate.source_identity_digest().cmp(&source_id))
            .ok()
            .and_then(|index| base.core_record_aggregates.get(index))
            .ok_or(IndexError::WriterInvariant(
                "source replacement is missing its base Core-record aggregate",
            ))?;
        let aggregate = match &pending.mode {
            PendingSourceMode::Replace => source_record_aggregate(
                &pending.source,
                pending.staged_documents,
                pending.core_record_accumulator,
            )?,
            PendingSourceMode::Retain { .. } => base_aggregate.clone(),
            PendingSourceMode::Append { base } => {
                if base_aggregate.indexed_documents() != base.counts().indexed_documents {
                    return Err(IndexError::CoreRecordAggregateCountMismatch {
                        source_id,
                        manifest: base.counts().indexed_documents,
                        index: base_aggregate.indexed_documents(),
                    });
                }
                let indexed_documents = base_aggregate
                    .indexed_documents()
                    .checked_add(pending.staged_documents)
                    .ok_or(IndexError::CountOverflow)?;
                let mut accumulator = base_aggregate.accumulator_bytes()?;
                accumulate_core_record(&mut accumulator, &pending.core_record_accumulator);
                source_record_aggregate(&pending.source, indexed_documents, accumulator)?
            }
        };
        if aggregate.indexed_documents() != certificate.counts().indexed_documents {
            return Err(IndexError::CoreRecordAggregateCountMismatch {
                source_id: source_token(&pending.source),
                manifest: certificate.counts().indexed_documents,
                index: aggregate.indexed_documents(),
            });
        }
        replacements.push((certificate.clone(), aggregate));
    }
    Ok(replacements)
}

/// Finishes a completed one-pass run when it reproduced the full verified
/// base manifest exactly.
///
/// Requiring every retained base source to have a pending certificate prevents
/// an incomplete route scan from turning a carried source into a false no-op.
/// An already-created Tantivy writer is rolled back; a writerless run is reused
/// directly. Physical work that leaves the logical corpus unchanged therefore
/// does not publish `meta.json`, advance the opstamp, or create a generation.
pub(super) fn finish_identical_staging<F, I>(
    generation: &mut GenerationWriter,
    manifest: &GenerationManifest,
    revalidate: &mut F,
    revalidate_inventory: &mut I,
) -> Result<bool>
where
    F: FnMut(RevalidationTarget<'_>) -> bool,
    I: FnMut(&CertifiedSourceInventory) -> bool,
{
    if !staged_manifest_matches_base(generation, manifest)? {
        return Ok(false);
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
        if !revalidate(RevalidationTarget::Deletion(&removal.proof)) {
            return Err(IndexError::SourceInvalidated(
                removal.source().identity().to_string(),
            ));
        }
    }
    for (route, revalidate_route) in &generation.route_publication_revalidations {
        if !revalidate_route() {
            return Err(IndexError::SourceInvalidated(route.as_str().to_owned()));
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

    generation
        .base_manifest()
        .ok_or(IndexError::WriterInvariant(
            "staged no-op is missing its verified base manifest",
        ))?;
    if let Some(mut writer) = generation.writer.take() {
        writer.rollback()?;
        writer.wait_merging_threads()?;
    }
    Ok(true)
}

fn staged_manifest_matches_base(
    generation: &GenerationWriter,
    manifest: &GenerationManifest,
) -> Result<bool> {
    if generation.writer.is_none()
        && generation.complete_inventories.is_empty()
        && generation.source_route_plan.is_none()
    {
        // A writerless candidate still needs current inventory authority. An
        // exact route plan plus the manifest equality and source coverage
        // checks below supplies it; otherwise preserve the ordinary path so
        // its terminal witness can reject races.
        return Ok(false);
    }
    let Some(base) = generation
        .base_publication
        .as_ref()
        .map(PinnedPublication::manifest)
    else {
        return Ok(false);
    };
    if base.sources.iter().any(|source| {
        !generation
            .pending
            .contains_key(&source_token(source.observation().source()))
            && !generation.source_is_carried_from_base(source.observation().source())
    }) {
        return Ok(false);
    }
    Ok(manifest.exact_snapshot_eq(base))
}
