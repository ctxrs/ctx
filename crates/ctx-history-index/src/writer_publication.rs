use super::*;
use crate::merge_policy::deletion_density_exceeds_limit;
use ctx_history_index_format::{
    open_publication_candidate, verify_and_bind_publication_candidate_with_progress,
    verify_and_bind_reusable_publication, CandidatePublicationVerificationError,
    ReusablePublicationError, VerifiedCandidatePublication,
};
use std::collections::BTreeMap;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static BASE_MANIFEST_SOURCE_MATERIALIZATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static SOURCE_REPLACEMENT_MANIFESTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn reset_manifest_materialization_visits() {
    BASE_MANIFEST_SOURCE_MATERIALIZATIONS.with(|visits| visits.set(0));
    PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS.with(|visits| visits.set(0));
    SOURCE_REPLACEMENT_MANIFESTS.with(|visits| visits.set(0));
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn manifest_materialization_visits() -> (u64, u64, u64) {
    (
        BASE_MANIFEST_SOURCE_MATERIALIZATIONS.with(std::cell::Cell::get),
        PARTIAL_BASE_ROUTE_MEMBER_MATERIALIZATIONS.with(std::cell::Cell::get),
        SOURCE_REPLACEMENT_MANIFESTS.with(std::cell::Cell::get),
    )
}

struct CommitGenerationOutcome {
    receipt: CommitReceipt,
    disposition: PublicationDisposition,
    verified_index: Option<VerifiedIndex>,
}

impl CommitGenerationOutcome {
    fn into_receipt(self) -> CommitReceipt {
        self.receipt
    }

    fn into_published_generation(self) -> Result<PublishedGeneration> {
        let verified_index = self.verified_index.ok_or(IndexError::WriterInvariant(
            "metadata publication completed without its verified index",
        ))?;
        PublishedGeneration::new(self.receipt, self.disposition, verified_index)
    }
}

struct VerifiedCandidate {
    slot: GenerationSlot,
    publication: VerifiedCandidatePublication,
}

impl GenerationWriter {
    /// Rebinds opaque owner metadata without changing the logical index payload.
    /// Current-format publications preserve their generation identity; a
    /// migrated publication first installs its required current manifest anchor.
    pub fn republish_current_publication_metadata(
        self,
        expected_generation_id: &str,
        publication_metadata: Vec<u8>,
    ) -> Result<VerifiedIndex> {
        self.ensure_reusable_base_not_invalidated()?;
        if self.preflight_lock.is_none() {
            return Err(IndexError::WriterInvariant(
                "generation writer lost its root publication lock",
            ));
        }
        let pointer = self
            .active_pointer
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "publication metadata republish requires an active generation",
            ))?;
        let base_publication =
            self.base_publication
                .as_ref()
                .ok_or(IndexError::WriterInvariant(
                    "publication metadata republish requires a base publication",
                ))?;
        let generation_id = base_publication.generation_id();
        let requires_current_manifest_anchor = base_publication.requires_current_manifest_anchor();
        if generation_id != expected_generation_id
            || pointer.active().generation_id() != expected_generation_id
        {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if publication_metadata.len() > MAX_PUBLICATION_METADATA_BYTES {
            return Err(IndexError::PublicationMetadataTooLarge {
                actual: publication_metadata.len(),
                maximum: MAX_PUBLICATION_METADATA_BYTES,
            });
        }
        if requires_current_manifest_anchor {
            let published =
                self.commit_with_publication_metadata(|_| true, move |_| Ok(publication_metadata))?;
            return Ok(published.into_parts().2);
        }
        let outcome = republish_current_with_publication_metadata(
            &self.root,
            pointer,
            &self.writer_options,
            publication_metadata.into(),
        )?;
        let published_pointer = match outcome {
            CurrentRepublishOutcome::Published(pointer) => pointer,
            CurrentRepublishOutcome::CommittedVisible { recovery, .. }
            | CurrentRepublishOutcome::CommittedRecoveryRequired { recovery } => {
                return Err(IndexError::CommittedGenerationNeedsRecovery {
                    generation_id: recovery.generation_id().to_owned(),
                    stage: "publication metadata republish",
                    detail: recovery.detail().to_owned(),
                });
            }
        };
        best_effort_post_republish_cleanup(&self.root, &published_pointer);
        let verified = VerifiedIndex::open_pinned(&self.root)?;
        if verified.generation_id() != expected_generation_id {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        Ok(verified)
    }

    pub(super) fn writer_mut(&mut self) -> Result<&mut IndexWriter<IndexDocument>> {
        if self.writer.is_none() {
            #[cfg(test)]
            if let Some(hook) = self.before_writer_handoff.take() {
                hook();
            }

            if self.candidate_directory_name.is_none() {
                let candidate = create_candidate_generation(
                    &self.root,
                    self.active_pointer
                        .as_ref()
                        .map(ActiveGenerationPointer::active),
                    self.writer_options.memory_bytes,
                )?;
                self.index = candidate.index;
                self.fields = fields_from_schema(&self.index.schema())?;
                validate_schema(&self.index.schema())?;
                self.candidate_directory_name = Some(candidate.directory_name);
                self.candidate_physical_proof = self
                    .active_pointer
                    .as_ref()
                    .map(|_| candidate.physical_proof);
                self.candidate_activation_fence = Some(candidate.activation_fence);
            }

            let writer = construct_index_writer_with_retry(&self.index, &self.writer_options)?;
            #[cfg(test)]
            self.index_writer_constructions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let current_metas = self.index.load_metas()?;
            let expected_generation = self
                .base_publication
                .as_ref()
                .map(PinnedPublication::generation_id);
            let current_generation = payload_generation_id(&current_metas)?;
            let expected_segments = self
                .base_publication
                .as_ref()
                .map(PinnedPublication::searcher)
                .map(searcher_generation)
                .unwrap_or_default();
            if current_metas.opstamp != self.base_opstamp
                || current_generation.as_deref() != expected_generation
                || meta_generation(&current_metas) != expected_segments
            {
                return Err(IndexError::ConcurrentGenerationChange);
            }

            writer.set_merge_policy(Box::new(LexicalMergePolicy::default()));
            let _ = writer.garbage_collect_files().wait()?;
            self.writer = Some(writer);
        }
        self.writer.as_mut().ok_or(IndexError::WriterInvariant(
            "lazy writer construction completed without a writer",
        ))
    }

    /// Prevents segment merging in tests without exposing the writer or its
    /// document type.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn test_disable_merges(&mut self) -> Result<()> {
        self.writer_mut()?
            .set_merge_policy(Box::<tantivy::indexer::NoMergePolicy>::default());
        Ok(())
    }

    /// Publishes one atomic lexical generation.
    ///
    /// `revalidate` runs after Tantivy has flushed all staged indexing workers
    /// and immediately before the immutable manifest and candidate commit.
    pub fn commit<F>(self, revalidate: F) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
    {
        Ok(self
            .commit_generation(revalidate, |_| false, |_| Ok(None), false, |_| Ok(()))?
            .into_receipt())
    }

    /// Publishes with refresh-owned opaque metadata constructed from the final
    /// terminally revalidated logical generation.
    ///
    /// Exact no-op/reuse does not invoke `metadata_factory`; callers must use
    /// [`PublicationDisposition`] to distinguish old generation metadata from
    /// bytes constructed for the current request.
    pub fn commit_with_publication_metadata<F, M>(
        self,
        revalidate: F,
        metadata_factory: M,
    ) -> Result<PublishedGeneration>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Vec<u8>>,
    {
        self.commit_generation(
            revalidate,
            |_| false,
            |context| metadata_factory(context).map(Some),
            true,
            |_| Ok(()),
        )?
        .into_published_generation()
    }

    /// Publishes one atomic lexical generation with terminal revalidation for
    /// each current complete-inventory certificate registered on the writer.
    pub fn commit_with_complete_inventory_revalidation<F, I>(
        self,
        revalidate: F,
        revalidate_inventory: I,
    ) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        Ok(self
            .commit_generation(
                revalidate,
                revalidate_inventory,
                |_| Ok(None),
                false,
                |_| Ok(()),
            )?
            .into_receipt())
    }

    /// Publishes with terminal source/inventory revalidation and a final
    /// refresh-owned opaque metadata factory.
    pub fn commit_with_complete_inventory_revalidation_and_publication_metadata<F, I, M>(
        self,
        revalidate: F,
        revalidate_inventory: I,
        metadata_factory: M,
    ) -> Result<PublishedGeneration>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Vec<u8>>,
    {
        self.commit_generation(
            revalidate,
            revalidate_inventory,
            |context| metadata_factory(context).map(Some),
            true,
            |_| Ok(()),
        )?
        .into_published_generation()
    }

    /// Publishes with terminal revalidation, owner metadata, and real
    /// whole-run publication stage transitions.
    pub fn commit_with_complete_inventory_revalidation_and_publication_metadata_and_progress<
        F,
        I,
        M,
        P,
    >(
        self,
        revalidate: F,
        revalidate_inventory: I,
        metadata_factory: M,
        report_progress: P,
    ) -> Result<PublishedGeneration>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Vec<u8>>,
        P: FnMut(PublicationStage) -> Result<()>,
    {
        self.commit_generation(
            revalidate,
            revalidate_inventory,
            |context| metadata_factory(context).map(Some),
            true,
            report_progress,
        )?
        .into_published_generation()
    }

    fn commit_generation<F, I, M, P>(
        mut self,
        mut revalidate: F,
        mut revalidate_inventory: I,
        metadata_factory: M,
        return_verified_index: bool,
        mut report_progress: P,
    ) -> Result<CommitGenerationOutcome>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Option<Vec<u8>>>,
        P: FnMut(PublicationStage) -> Result<()>,
    {
        self.ensure_reusable_base_not_invalidated()?;
        if self.preflight_lock.is_none() {
            return Err(IndexError::WriterInvariant(
                "generation writer lost its root publication lock",
            ));
        }
        self.validate_source_route_plan_complete()?;
        if let Some(witness) = self.exact_replay_inventory_witness()? {
            for route in witness.base.source_routes().iter().filter(|route| {
                !self
                    .source_route_plan
                    .as_ref()
                    .is_some_and(|plan| plan.carried_from_base.contains(route.route_identity()))
                    && !self
                        .partially_reconciled_routes
                        .contains(route.route_identity())
            }) {
                for source in route.sources() {
                    let certificate = witness
                        .base
                        .sources
                        .binary_search_by_key(&source.identity().digest(), |candidate| {
                            candidate.observation().source().identity().digest()
                        })
                        .ok()
                        .and_then(|index| witness.base.sources.get(index))
                        .ok_or(IndexError::WriterInvariant(
                            "validated route member is missing its source certificate",
                        ))?;
                    if !revalidate(RevalidationTarget::Source(certificate)) {
                        return Err(IndexError::SourceInvalidated(
                            certificate.observation().source().identity().to_string(),
                        ));
                    }
                }
            }
            for inventory in &self.complete_inventories {
                if !revalidate_inventory(inventory) {
                    return Err(IndexError::CompleteInventoryInvalidated {
                        provider: inventory.observation().provider().to_owned(),
                        authority_namespace: inventory
                            .observation()
                            .authority_namespace()
                            .to_owned(),
                    });
                }
            }
            for (route, revalidate_route) in &self.route_publication_revalidations {
                if !revalidate_route() {
                    return Err(IndexError::SourceInvalidated(route.as_str().to_owned()));
                }
            }
            let opstamp = self.base_opstamp;
            report_progress(PublicationStage::PhysicalVerification)?;
            let reused = self.reused_generation(opstamp, return_verified_index)?;
            report_progress(PublicationStage::Activation)?;
            return Ok(reused);
        }

        for pending in self.pending.values() {
            if pending.certificate.is_none() {
                return Err(IndexError::SourceNotCertified(
                    pending.source.identity().to_string(),
                ));
            }
        }

        let manifest = std::sync::Arc::new(self.next_manifest()?);
        if finish_identical_staging(
            &mut self,
            &manifest,
            &mut revalidate,
            &mut revalidate_inventory,
        )? {
            self.discard_candidate()?;
            let opstamp = self.base_opstamp;
            report_progress(PublicationStage::PhysicalVerification)?;
            let reused = self.reused_generation(opstamp, return_verified_index)?;
            report_progress(PublicationStage::Activation)?;
            return Ok(reused);
        }

        // A provider-source configuration change can be the only manifest
        // mutation (for example, disabling automatic discovery while carrying
        // the previously indexed routes). Materialize a candidate so that
        // this metadata-only successor is published atomically as well.
        self.writer_mut()?;

        // Build opaque owner metadata from the complete staged manifest before
        // the terminal source fence. The bytes are bound only if every source
        // and inventory revalidation below succeeds, so observations sampled
        // by the owner cannot describe state newer than the Core projection
        // that the fence accepts.
        let prepared_manifest = prepare_successor_manifest(
            &self.root,
            std::sync::Arc::clone(&manifest),
            self.base_publication
                .as_ref()
                .map(|base| (base.generation_id(), base.manifest())),
        )?;
        let generation_id = prepared_manifest.generation_id().to_owned();
        let publication_metadata =
            metadata_factory(PublicationMetadataContext::new(&generation_id, &manifest))?;

        self.apply_route_deletions()?;
        let candidate_path = self.candidate_path()?;
        let previous_generation_id = self
            .base_publication
            .as_ref()
            .map(PinnedPublication::generation_id)
            .map(str::to_owned);
        let root = self.root.clone();
        let mut prepared = self
            .writer
            .as_mut()
            .ok_or(IndexError::WriterInvariant(
                "mutating commit is missing its lazy writer",
            ))?
            .prepare_commit()?;
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            if !revalidate(RevalidationTarget::Source(certificate)) {
                let source = pending.source.identity().to_string();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(source));
            }
        }
        for removal in self.deletions.values() {
            if !revalidate(RevalidationTarget::Deletion(&removal.proof)) {
                let source = removal.source().identity().to_string();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(source));
            }
        }
        for (route, revalidate_route) in &self.route_publication_revalidations {
            if !revalidate_route() {
                let route = route.as_str().to_owned();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(route));
            }
        }
        for inventory in &self.complete_inventories {
            if !revalidate_inventory(inventory) {
                let error = IndexError::CompleteInventoryInvalidated {
                    provider: inventory.observation().provider().to_owned(),
                    authority_namespace: inventory.observation().authority_namespace().to_owned(),
                };
                prepared.abort()?;
                return Err(error);
            }
        }

        let payload =
            match canonical_commit_payload(&generation_id, publication_metadata.as_deref()) {
                Ok(payload) => payload,
                Err(error) => {
                    prepared.abort()?;
                    return Err(error);
                }
            };
        if let Err(error) = write_prepared_manifest(&root, &prepared_manifest) {
            let _ = prepared.abort();
            return Err(error);
        }
        prepared.set_payload(&payload);
        #[cfg(test)]
        if let Some(hook) = self.before_candidate_commit.take() {
            hook(&candidate_path);
        }
        report_progress(PublicationStage::Merging)?;
        let commit_result = prepared.commit();
        #[cfg(test)]
        let commit_result = if self.return_commit_error_after_visibility {
            commit_result.and_then(|_| {
                Err(tantivy::TantivyError::InvalidArgument(
                    "injected error after the candidate commit became visible".to_owned(),
                ))
            })
        } else {
            commit_result
        };
        drop(payload);
        drop(publication_metadata);
        let writer = self.writer.take().ok_or(IndexError::WriterInvariant(
            "candidate commit is missing its lazy writer",
        ))?;
        writer.wait_merging_threads()?;
        let (opstamp, reconciled_commit_error) = match commit_result {
            Ok(opstamp) => (opstamp, None),
            Err(error) => {
                let commit_error = error.to_string();
                let opstamp = reconcile_commit_error(
                    &self.index,
                    &generation_id,
                    previous_generation_id.as_deref(),
                    error,
                )?;
                (opstamp, Some(commit_error))
            }
        };
        // Merge completion fixes the exact writer-produced segment and delete
        // topology. Verification may rely on canonical staging only while this
        // ephemeral fence still matches the bytes it is about to publish.
        let committed_candidate_generation = meta_generation(&self.index.load_metas()?);

        #[cfg(test)]
        if let Some(hook) = self.after_candidate_commit.take() {
            hook(&candidate_path);
        }
        #[cfg(test)]
        if let Some(hook) = self.before_pointer_switch.take() {
            hook(&candidate_path);
        }
        report_progress(PublicationStage::Syncing)?;
        sync_generation(&candidate_path)?;
        if let Some(proof) = self.candidate_physical_proof.as_mut() {
            prime_candidate_physical_proof(
                &self.index,
                &candidate_path,
                self.active_pointer.as_ref(),
                proof,
            )?;
        }
        validate_candidate_managed_files(
            &self.index,
            &candidate_path,
            self.active_pointer.as_ref(),
        )?;

        let directory_name =
            self.candidate_directory_name
                .clone()
                .ok_or(IndexError::WriterInvariant(
                    "verified candidate has no generation directory",
                ))?;
        report_progress(PublicationStage::PhysicalVerification)?;
        let verified = self
            .verify_candidate(
                &candidate_path,
                &generation_id,
                &directory_name,
                &committed_candidate_generation,
                || report_progress(PublicationStage::LogicalVerification),
            )
            .map_err(
                |verification_error| match reconciled_commit_error.as_ref() {
                    None => verification_error,
                    Some(commit_error) => IndexError::CommittedGenerationNeedsRecovery {
                        generation_id: generation_id.clone(),
                        stage: "candidate commit reconciliation",
                        detail: format!(
                            "{commit_error}; candidate commit completed but verification failed: \
                             {verification_error}"
                        ),
                    },
                },
            )?;
        drop(manifest);
        let next_pointer = ActiveGenerationPointer::new(
            verified.slot.clone(),
            self.base_publication.as_ref().and_then(|_| {
                self.active_pointer
                    .as_ref()
                    .map(|pointer| pointer.active().clone())
            }),
        )?;
        #[cfg(test)]
        if let Some(hook) = self.before_pointer_publication.take() {
            hook(&candidate_path);
        }
        let activation_fence =
            self.candidate_activation_fence
                .as_ref()
                .ok_or(IndexError::WriterInvariant(
                    "verified candidate has no activation fence",
                ))?;
        let terminal_index = verified.publication.publication().searcher().index();
        let expected_audit = verified.publication.physical_integrity_audit();
        #[cfg(windows)]
        let mut terminal_guard = Some(
            ctx_history_index_generation::acquire_terminal_publication_guard(
                &root,
                &candidate_path,
                terminal_index,
                self.active_pointer.as_ref(),
            )?,
        );
        certify_candidate_physical_integrity(
            &root,
            &self.active_pointer_fence,
            &verified.slot,
            terminal_index,
            expected_audit,
        )?;
        let reopened_candidate = VerifiedIndex::open_certified_candidate_before_activation(
            &root,
            &self.active_pointer_fence,
            &verified.slot,
        )?;
        report_progress(PublicationStage::Activation)?;
        match publish_active_generation_pointer_validated(&root, &next_pointer, || {
            activation_fence.validate_binding()?;
            prepared_manifest
                .verify_persisted(&root)
                .map_err(|_| ctx_history_index_generation::GenerationError::ChecksumMismatch)?;
            validate_candidate_managed_files(
                terminal_index,
                &candidate_path,
                self.active_pointer.as_ref(),
            )?;
            ctx_history_index_generation::verify_candidate_physical_integrity_read_only(
                &root,
                &self.active_pointer_fence,
                &verified.slot,
                terminal_index,
            )?;
            #[cfg(windows)]
            terminal_guard
                .as_ref()
                .ok_or(ctx_history_index_generation::GenerationError::ConcurrentGenerationChange)?
                .verify_physical_fence(expected_audit)?;
            activation_fence.validate_binding()?;
            #[cfg(windows)]
            {
                let terminal_guard = terminal_guard.take().ok_or(
                    ctx_history_index_generation::GenerationError::ConcurrentGenerationChange,
                )?;
                terminal_guard.verify_identities()?;
                Ok(terminal_guard)
            }
            #[cfg(not(windows))]
            Ok(())
        }) {
            Ok(PointerPublicationOutcome::Durable) => {}
            Ok(PointerPublicationOutcome::CommittedVisible { detail }) => {
                return Err(IndexError::CommittedGenerationNeedsRecovery {
                    generation_id,
                    stage: "active generation pointer durability",
                    detail,
                });
            }
            Err(error) => {
                return Err(self.classify_pointer_failure(&generation_id, &next_pointer, error));
            }
        }
        #[cfg(test)]
        if let Some(hook) = self.after_pointer_switch.take() {
            hook(&candidate_path);
        }
        // The durable pointer is authoritative now. Writer open retries every
        // cleanup below, so treat each attempt independently and never turn a
        // published generation into a failed refresh because reclamation was
        // temporarily obstructed. A malformed lease suppresses every reclaim:
        // treating it as absent could delete the one target it was meant to
        // preserve before the next strict writer open reports it.
        let _ = clear_active_generation_rebuild_marker(&root);
        if let Ok(retention_lease) = load_generation_retention_lease(&root) {
            let mut retained_generation_ids = std::iter::once(next_pointer.active())
                .chain(next_pointer.previous())
                .map(|slot| slot.generation_id().to_owned())
                .collect::<Vec<_>>();
            retained_generation_ids.extend(
                retention_lease
                    .as_ref()
                    .map(|lease| lease.generation_id().to_owned()),
            );
            let _ = reclaim_inactive_generation_directories(
                &root,
                Some(&next_pointer),
                retention_lease.as_ref(),
            );
            let _ = reclaim_unreferenced_manifests(&root, &retained_generation_ids);
            let _ = reclaim_unreferenced_certifications(
                &root,
                Some(&next_pointer),
                retention_lease.as_ref(),
            );
        }
        let receipt = CommitReceipt::from_verified_manifest(
            opstamp,
            generation_id.clone(),
            std::sync::Arc::clone(verified.publication.publication().shared_manifest()),
        );
        let verified_index = return_verified_index.then_some(reopened_candidate);
        Ok(CommitGenerationOutcome {
            receipt,
            disposition: PublicationDisposition::Published,
            verified_index,
        })
    }

    fn apply_route_deletions(&mut self) -> Result<()> {
        let source_key_field = self.fields.source_key;
        let tokens = self
            .route_deletions
            .iter()
            .map(source_token)
            .collect::<Vec<_>>();
        let writer = self.writer_mut()?;
        for token in tokens {
            writer.delete_term(Term::from_field_text(source_key_field, &token));
        }
        Ok(())
    }

    fn ensure_reusable_base_not_invalidated(&self) -> Result<()> {
        let Some(detail) = self.reusable_base_rebuild_detail.as_ref() else {
            return Ok(());
        };
        let active = self
            .active_pointer
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "invalidated reusable base is missing its active pointer",
            ))?
            .active();
        Err(IndexError::ActiveGenerationNeedsRebuild {
            generation_id: active.generation_id().to_owned(),
            detail: detail.clone(),
        })
    }

    fn verify_candidate<P>(
        &self,
        candidate_path: &Path,
        generation_id: &str,
        directory_name: &str,
        committed_candidate_generation: &BTreeMap<String, Option<u64>>,
        report_logical_verification: P,
    ) -> Result<VerifiedCandidate>
    where
        P: FnOnce() -> Result<()>,
    {
        let candidate = open_publication_candidate(&self.root, candidate_path)?;
        if candidate.generation_id() != generation_id {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if &meta_generation(candidate.metas()) != committed_candidate_generation {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        for segment in &candidate.metas().segments {
            if deletion_density_exceeds_limit(segment) {
                return Err(IndexError::CandidateDeletionDensityExceeded {
                    deleted_documents: u64::from(segment.num_deleted_docs()),
                    max_documents: u64::from(segment.max_doc()),
                });
            }
        }
        let publication = verify_and_bind_publication_candidate_with_progress(
            candidate,
            self.active_pointer.as_ref(),
            self.base_publication.as_ref(),
            self.active_pointer
                .as_ref()
                .map(|pointer| (&*self.root, pointer, pointer.active())),
            self.candidate_physical_proof.as_ref(),
            report_logical_verification,
        )
        .map_err(|error| match error {
            CandidatePublicationVerificationError::Candidate(error) => error,
            CandidatePublicationVerificationError::Reusable(ReusablePublicationError::Binding(
                error,
            )) => error,
            CandidatePublicationVerificationError::Reusable(
                ReusablePublicationError::Integrity(error),
            ) => {
                let active = self
                    .active_pointer
                    .as_ref()
                    .expect("reusable publication verification has active authority")
                    .active();
                classify_active_integrity_failure(&self.root, active, error)
            }
        })?;
        let slot = GenerationSlot::new(
            generation_id.to_owned(),
            directory_name.to_owned(),
            publication.physical_integrity_audit().digest().to_owned(),
        )?;
        Ok(VerifiedCandidate { slot, publication })
    }

    fn reused_generation(
        mut self,
        opstamp: u64,
        return_verified_index: bool,
    ) -> Result<CommitGenerationOutcome> {
        let base = self
            .base_publication
            .take()
            .ok_or(IndexError::WriterInvariant(
                "no-op integrity validation is missing its base publication",
            ))?;
        let pointer = self
            .active_pointer
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "no-op integrity validation is missing its active pointer",
            ))?;
        let active = pointer.active();
        if active.generation_id() != base.generation_id() {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let publication = verify_and_bind_reusable_publication(&self.root, pointer, active, base)
            .map_err(|error| match error {
            ReusablePublicationError::Binding(error) => error,
            ReusablePublicationError::Integrity(error) => {
                self.classify_reusable_integrity_failure(active, error)
            }
        })?;
        let receipt = CommitReceipt::from_verified_manifest(
            opstamp,
            publication.generation_id().to_owned(),
            std::sync::Arc::clone(publication.shared_manifest()),
        );
        let verified_index =
            return_verified_index.then(|| VerifiedIndex::from_verified_publication(publication));
        Ok(CommitGenerationOutcome {
            receipt,
            disposition: PublicationDisposition::Reused,
            verified_index,
        })
    }

    fn classify_reusable_integrity_failure(
        &self,
        active: &GenerationSlot,
        error: IndexError,
    ) -> IndexError {
        classify_active_integrity_failure(&self.root, active, error)
    }

    fn classify_pointer_failure(
        &self,
        generation_id: &str,
        expected: &ActiveGenerationPointer,
        error: IndexError,
    ) -> IndexError {
        match load_active_generation_pointer(&self.root) {
            Ok(Some(pointer)) if &pointer == expected => {
                IndexError::CommittedGenerationNeedsRecovery {
                    generation_id: generation_id.to_owned(),
                    stage: "active generation pointer durability",
                    detail: error.to_string(),
                }
            }
            Ok(pointer) if pointer == self.active_pointer => error,
            Ok(pointer) => IndexError::CommittedGenerationNeedsRecovery {
                generation_id: generation_id.to_owned(),
                stage: "active generation pointer reconciliation",
                detail: format!("{error}; active pointer is {pointer:?}"),
            },
            Err(reconcile_error) => IndexError::CommittedGenerationNeedsRecovery {
                generation_id: generation_id.to_owned(),
                stage: "active generation pointer reconciliation",
                detail: format!("{error}; pointer reload failed: {reconcile_error}"),
            },
        }
    }

    fn candidate_path(&self) -> Result<PathBuf> {
        let directory =
            self.candidate_directory_name
                .as_deref()
                .ok_or(IndexError::WriterInvariant(
                    "candidate generation directory is missing",
                ))?;
        Ok(self.root.join(INDEX_GENERATIONS_DIRECTORY).join(directory))
    }

    fn discard_candidate(&mut self) -> Result<()> {
        let Some(directory) = self.candidate_directory_name.take() else {
            return Ok(());
        };
        if let Some(proof) = self.candidate_physical_proof.as_mut() {
            proof.clear();
        }
        self.candidate_physical_proof = None;
        let activation_fence =
            self.candidate_activation_fence
                .take()
                .ok_or(IndexError::WriterInvariant(
                    "candidate generation is missing its activation fence",
                ))?;
        activation_fence.validate_binding()?;
        fs::remove_dir_all(self.root.join(INDEX_GENERATIONS_DIRECTORY).join(directory))?;
        sync_directory(&self.root.join(INDEX_GENERATIONS_DIRECTORY))?;
        Ok(())
    }
}

mod manifest_merge;
mod manifest_planning;

use manifest_merge::{merge_manifest_sources, merge_partial_route_members};
