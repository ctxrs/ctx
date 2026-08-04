use super::*;
use crate::merge_policy::deletion_density_exceeds_limit;
use std::collections::BTreeMap;

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
    searcher: Searcher,
    manifest: std::sync::Arc<GenerationManifest>,
    publication_metadata: Option<std::sync::Arc<[u8]>>,
    physical_integrity_audit: PhysicalIntegrityAudit,
}

impl GenerationWriter {
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
                )?;
                self.index = candidate.index;
                self.fields = fields_from_schema(&self.index.schema())?;
                validate_schema(&self.index.schema())?;
                self.candidate_directory_name = Some(candidate.directory_name);
            }

            let writer = construct_index_writer_with_retry(&self.index, &self.writer_options)?;
            #[cfg(test)]
            self.index_writer_constructions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let current_metas = self.index.load_metas()?;
            let expected_generation = self
                .base_manifest
                .as_ref()
                .map(GenerationManifest::generation_id)
                .transpose()?;
            let current_generation = payload_generation_id(&current_metas)?;
            let expected_segments = self
                .base_searcher
                .as_ref()
                .map(searcher_generation)
                .unwrap_or_default();
            if current_metas.opstamp != self.base_opstamp
                || current_generation != expected_generation
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

    /// Publishes one atomic lexical generation.
    ///
    /// `revalidate` runs after Tantivy has flushed all staged indexing workers
    /// and immediately before the immutable manifest and candidate commit.
    pub fn commit<F>(self, revalidate: F) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
    {
        Ok(self
            .commit_generation(revalidate, |_| false, |_| Ok(None), false)?
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
            .commit_generation(revalidate, revalidate_inventory, |_| Ok(None), false)?
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
        )?
        .into_published_generation()
    }

    fn commit_generation<F, I, M>(
        mut self,
        mut revalidate: F,
        mut revalidate_inventory: I,
        metadata_factory: M,
        return_verified_index: bool,
    ) -> Result<CommitGenerationOutcome>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
        M: FnOnce(PublicationMetadataContext<'_>) -> Result<Option<Vec<u8>>>,
    {
        if self.preflight_lock.is_none() {
            return Err(IndexError::WriterInvariant(
                "generation writer lost its root publication lock",
            ));
        }
        self.validate_source_route_plan_complete()?;
        if let Some(witness) = self.exact_replay_inventory_witness()? {
            for certificate in witness.base.sources.iter().filter(|certificate| {
                !self.source_is_carried_from_base(certificate.observation().source())
            }) {
                if !revalidate(RevalidationTarget::Source(certificate)) {
                    return Err(IndexError::SourceInvalidated(
                        certificate.observation().source().identity().to_string(),
                    ));
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
            self.validate_base_integrity_for_reuse()?;
            let receipt = CommitReceipt::from_manifest(self.base_opstamp, witness.base.clone())?;
            return self.reused_generation(receipt, return_verified_index);
        }

        for pending in self.pending.values() {
            if pending.certificate.is_none() {
                return Err(IndexError::SourceNotCertified(
                    pending.source.identity().to_string(),
                ));
            }
        }

        let manifest = self.next_manifest()?;
        if let Some(receipt) = finish_identical_staging(
            &mut self,
            &manifest,
            &mut revalidate,
            &mut revalidate_inventory,
        )? {
            self.discard_candidate()?;
            self.validate_base_integrity_for_reuse()?;
            return self.reused_generation(receipt, return_verified_index);
        }

        // Build opaque owner metadata from the complete staged manifest before
        // the terminal source fence. The bytes are bound only if every source
        // and inventory revalidation below succeeds, so observations sampled
        // by the owner cannot describe state newer than the Core projection
        // that the fence accepts.
        let generation_id = manifest.generation_id()?;
        let publication_metadata =
            metadata_factory(PublicationMetadataContext::new(&generation_id, &manifest))?;

        self.writer_mut()?;
        let candidate_path = self.candidate_path()?;
        let previous_generation_id = self
            .base_manifest
            .as_ref()
            .map(GenerationManifest::generation_id)
            .transpose()?;
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
        if let Err(error) = write_manifest(&root, &generation_id, &manifest) {
            let _ = prepared.abort();
            return Err(error);
        }
        prepared.set_payload(&payload);
        #[cfg(test)]
        if let Some(hook) = self.before_candidate_commit.take() {
            hook(&candidate_path);
        }
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
        sync_generation(&candidate_path)?;

        let directory_name =
            self.candidate_directory_name
                .clone()
                .ok_or(IndexError::WriterInvariant(
                    "verified candidate has no generation directory",
                ))?;
        let verified = self
            .verify_candidate(
                &candidate_path,
                &manifest,
                &generation_id,
                &directory_name,
                &committed_candidate_generation,
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
            self.base_manifest.as_ref().and_then(|_| {
                self.active_pointer
                    .as_ref()
                    .map(|pointer| pointer.active().clone())
            }),
        )?;
        #[cfg(test)]
        if let Some(hook) = self.before_pointer_publication.take() {
            hook(&candidate_path);
        }
        match publish_active_generation_pointer(&root, &next_pointer) {
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
        let retained_generation_ids = std::iter::once(next_pointer.active())
            .chain(next_pointer.previous())
            .map(|slot| slot.generation_id().to_owned())
            .collect::<Vec<_>>();
        // The durable pointer is authoritative now. Writer open retries every
        // cleanup below, so treat each attempt independently and never turn a
        // published generation into a failed refresh because reclamation was
        // temporarily obstructed.
        let _ = clear_active_generation_rebuild_marker(&root);
        let _ = reclaim_inactive_generation_directories(&root, Some(&next_pointer));
        let _ = reclaim_unreferenced_manifests(&root, &retained_generation_ids);
        let _ = reclaim_unreferenced_certifications(&root, Some(&next_pointer));
        let _ = publication::certify_activated_generation(
            &root,
            &next_pointer,
            next_pointer.active(),
            verified.searcher.index(),
            &verified.physical_integrity_audit,
        );

        let receipt = CommitReceipt::from_verified_manifest(
            opstamp,
            generation_id.clone(),
            std::sync::Arc::clone(&verified.manifest),
        );
        let verified_index = return_verified_index.then(|| {
            VerifiedIndex::from_verified_publication(
                verified.searcher,
                verified.manifest,
                generation_id,
                verified.publication_metadata,
            )
        });
        Ok(CommitGenerationOutcome {
            receipt,
            disposition: PublicationDisposition::Published,
            verified_index,
        })
    }

    fn verify_candidate(
        &self,
        candidate_path: &Path,
        manifest: &GenerationManifest,
        generation_id: &str,
        directory_name: &str,
        committed_candidate_generation: &BTreeMap<String, Option<u64>>,
    ) -> Result<VerifiedCandidate> {
        let directory =
            DurableMmapDirectory::open(candidate_path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        validate_schema(&index.schema())?;
        if index.settings() != &publication::lexical_index_settings() {
            return Err(IndexError::IndexSettingsMismatch(LEXICAL_SCHEMA_VERSION));
        }
        let metas = index.load_metas()?;
        let loaded_publication = load_publication_for_metas(&self.root, &metas)?;
        if loaded_publication.generation_id != generation_id {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if &meta_generation(&metas) != committed_candidate_generation {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        for segment in &metas.segments {
            if deletion_density_exceeds_limit(segment) {
                return Err(IndexError::CandidateDeletionDensityExceeded {
                    deleted_documents: u64::from(segment.num_deleted_docs()),
                    max_documents: u64::from(segment.max_doc()),
                });
            }
        }
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let physical_integrity_audit = physical_integrity_audit(&index, candidate_path)?;
        verify_publication_candidate(&searcher, manifest, self.base_searcher.as_ref())?;
        let slot = GenerationSlot::new(
            generation_id.to_owned(),
            directory_name.to_owned(),
            physical_integrity_audit.digest().to_owned(),
        )?;
        Ok(VerifiedCandidate {
            slot,
            searcher,
            manifest: std::sync::Arc::new(loaded_publication.manifest),
            publication_metadata: loaded_publication.metadata,
            physical_integrity_audit,
        })
    }

    fn reused_generation(
        &self,
        receipt: CommitReceipt,
        return_verified_index: bool,
    ) -> Result<CommitGenerationOutcome> {
        let verified_index = if return_verified_index {
            let searcher = self
                .base_searcher
                .clone()
                .ok_or(IndexError::WriterInvariant(
                    "reused generation is missing its pinned base searcher",
                ))?;
            Some(VerifiedIndex::from_verified_publication(
                searcher,
                receipt.shared_manifest(),
                receipt.generation_id.clone(),
                self.base_publication_metadata.clone(),
            ))
        } else {
            None
        };
        Ok(CommitGenerationOutcome {
            receipt,
            disposition: PublicationDisposition::Reused,
            verified_index,
        })
    }

    fn validate_base_integrity_for_reuse(&self) -> Result<()> {
        let base = self
            .base_manifest
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "no-op integrity validation is missing its base manifest",
            ))?;
        let generation_id = base.generation_id()?;
        let active = self
            .active_pointer
            .as_ref()
            .map(ActiveGenerationPointer::active)
            .ok_or(IndexError::WriterInvariant(
                "no-op integrity validation is missing its active generation",
            ))?;
        if active.generation_id() != generation_id {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let active_index = open_slot_index(&self.root, active)?;
        let pointer = self
            .active_pointer
            .as_ref()
            .ok_or(IndexError::WriterInvariant(
                "no-op integrity validation is missing its active pointer",
            ))?;
        if let Err(error) =
            verify_or_certify_physical_integrity(&self.root, pointer, active, &active_index)
        {
            let detail =
                match writer_support::mark_active_generation_for_rebuild(&self.root, active) {
                    Ok(()) => error.to_string(),
                    Err(marker_error) => format!(
                        "{error}; persisting the rebuild decision also failed: {marker_error}"
                    ),
                };
            return Err(IndexError::ActiveGenerationNeedsRebuild {
                generation_id,
                detail,
            });
        }
        Ok(())
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
        fs::remove_dir_all(self.root.join(INDEX_GENERATIONS_DIRECTORY).join(directory))?;
        sync_directory(&self.root.join(INDEX_GENERATIONS_DIRECTORY))?;
        Ok(())
    }

    fn next_manifest(&self) -> Result<GenerationManifest> {
        self.validate_source_route_plan_complete()?;
        let mut sources = HashMap::<SourceKey, CertifiedSource>::new();
        if let Some(base) = &self.base_manifest {
            for source in &base.sources {
                sources.insert(source.observation().source().clone(), source.clone());
            }
        }
        for source in self.deletions.keys().chain(&self.route_deletions) {
            sources.remove(source);
        }
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            sources.insert(pending.source.clone(), certificate.clone());
        }
        let sources = sources.into_values().collect::<Vec<_>>();
        let record_aggregates = staging::manifest_record_aggregates(self, &sources)?;
        let mut source_routes = if let Some(routes) = &self.present_source_routes {
            routes.clone()
        } else {
            contracts::implicit_source_routes(&sources)?
        };
        source_routes.extend(self.observed_missing_routes.values().cloned());
        GenerationManifest::from_parts_with_record_aggregates(
            sources,
            record_aggregates,
            source_routes,
        )
    }
}
