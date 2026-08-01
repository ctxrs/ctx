use super::*;

impl GenerationWriter {
    pub(super) fn writer_mut(&mut self) -> Result<&mut IndexWriter<IndexDocument>> {
        if self.writer.is_none() {
            #[cfg(test)]
            if let Some(hook) = self.before_writer_handoff.take() {
                hook();
            }

            if self.candidate_directory_name.is_none() {
                if self.base_manifest.is_some() {
                    // Validate before cloning: hard-linking immutable segment
                    // files legitimately changes their inode ctime on Unix.
                    self.validate_base_integrity_for_reuse()?;
                }
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

            let mut merge_policy = LogMergePolicy::default();
            merge_policy.set_min_num_segments(LEXICAL_SEGMENT_MERGE_FAN_IN);
            writer.set_merge_policy(Box::new(merge_policy));
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
        self.commit_with_complete_inventory_revalidation(revalidate, |_| false)
    }

    /// Publishes one atomic lexical generation with terminal revalidation for
    /// each current complete-inventory certificate registered on the writer.
    pub fn commit_with_complete_inventory_revalidation<F, I>(
        mut self,
        mut revalidate: F,
        mut revalidate_inventory: I,
    ) -> Result<CommitReceipt>
    where
        F: FnMut(RevalidationTarget<'_>) -> bool,
        I: FnMut(&CertifiedSourceInventory) -> bool,
    {
        if self.preflight_lock.is_none() {
            return Err(IndexError::WriterInvariant(
                "generation writer lost its root publication lock",
            ));
        }
        if let Some(witness) = self.exact_replay_inventory_witness()? {
            self.validate_base_integrity_for_reuse()?;
            for certificate in &witness.base.sources {
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
            return CommitReceipt::from_manifest(self.base_opstamp, witness.base.clone());
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
            return Ok(receipt);
        }

        self.writer_mut()?;
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
            if !revalidate(RevalidationTarget::Deletion(removal.deletion())) {
                let source = removal.source().identity().to_string();
                prepared.abort()?;
                return Err(IndexError::SourceInvalidated(source));
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

        let generation_id = manifest.generation_id()?;
        if let Err(error) = write_manifest(&root, &generation_id, &manifest) {
            let _ = prepared.abort();
            return Err(error);
        }
        let payload = serde_json::to_string(&CommitPayload {
            version: COMMIT_PAYLOAD_VERSION,
            generation_id: generation_id.clone(),
        })?;
        prepared.set_payload(&payload);
        let commit_result = prepared.commit();
        let writer = self.writer.take().ok_or(IndexError::WriterInvariant(
            "candidate commit is missing its lazy writer",
        ))?;
        writer.wait_merging_threads()?;
        let opstamp = match commit_result {
            Ok(opstamp) => opstamp,
            Err(error) => reconcile_commit_error(
                &self.index,
                &root,
                &generation_id,
                previous_generation_id.as_deref(),
                error,
            )?,
        };

        let candidate_path = self.candidate_path()?;
        #[cfg(test)]
        if let Some(hook) = self.after_candidate_commit.take() {
            hook(&candidate_path);
        }
        sync_generation(&candidate_path)?;
        #[cfg(test)]
        if let Some(hook) = self.before_pointer_switch.take() {
            hook(&candidate_path);
        }
        self.verify_candidate(&candidate_path, &manifest, &generation_id)?;
        writer_support::write_generation_integrity_receipt(&root, &generation_id, &candidate_path)?;

        let directory_name =
            self.candidate_directory_name
                .clone()
                .ok_or(IndexError::WriterInvariant(
                    "verified candidate has no generation directory",
                ))?;
        let active = GenerationSlot::new(generation_id.clone(), directory_name)?;
        let next_pointer = ActiveGenerationPointer::new(
            active,
            self.base_manifest.as_ref().and_then(|_| {
                self.active_pointer
                    .as_ref()
                    .map(|pointer| pointer.active().clone())
            }),
        )?;
        if let Err(error) = publish_active_generation_pointer(&root, &next_pointer) {
            return Err(self.classify_pointer_failure(&generation_id, &next_pointer, error));
        }
        #[cfg(test)]
        if let Some(hook) = self.after_pointer_switch.take() {
            hook(&candidate_path);
        }
        let retained_generation_ids = std::iter::once(next_pointer.active())
            .chain(next_pointer.previous())
            .map(|slot| slot.generation_id().to_owned())
            .collect::<Vec<_>>();
        let retained_generation_directories = std::iter::once(next_pointer.active())
            .chain(next_pointer.previous())
            .map(|slot| slot.directory().to_owned())
            .collect::<Vec<_>>();
        // The durable pointer is authoritative now. Writer open retries every
        // cleanup below, so treat each attempt independently and never turn a
        // published generation into a failed refresh because reclamation was
        // temporarily obstructed.
        let _ = clear_active_generation_rebuild_marker(&root);
        let _ = reclaim_inactive_generation_directories(&root, Some(&next_pointer));
        let _ = reclaim_unreferenced_manifests(&root, &retained_generation_ids);
        let _ = writer_support::reclaim_generation_integrity_receipts(
            &root,
            &retained_generation_directories,
        );

        CommitReceipt::from_manifest(opstamp, manifest)
    }

    fn verify_candidate(
        &self,
        candidate_path: &Path,
        manifest: &GenerationManifest,
        generation_id: &str,
    ) -> Result<()> {
        let directory =
            DurableMmapDirectory::open(candidate_path).map_err(tantivy::TantivyError::from)?;
        let index = Index::open(directory)?;
        validate_schema(&index.schema())?;
        if index.settings() != &publication::lexical_index_settings() {
            return Err(IndexError::IndexSettingsMismatch(LEXICAL_SCHEMA_VERSION));
        }
        let metas = index.load_metas()?;
        if payload_generation_id(&metas)?.as_deref() != Some(generation_id) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let loaded_manifest = load_manifest_for_metas(&self.root, &metas)?;
        if loaded_manifest.generation_id()? != generation_id {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        if searcher_generation(&searcher) != meta_generation(&metas) {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        verify_searcher_structure(&searcher, manifest)?;
        publication::verify_event_id_terms(&searcher, manifest)
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
        let generation_path = self
            .root
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(active.directory());
        if let Err(error) = writer_support::validate_generation_integrity_receipt(
            &self.root,
            &generation_id,
            &generation_path,
        ) {
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
        let mut sources = HashMap::<SourceKey, CertifiedSource>::new();
        let mut removals = HashMap::<SourceKey, GenerationRemoval>::new();
        let mut missing_sources = HashMap::<SourceKey, SourceCatalogMissingState>::new();
        if let Some(base) = &self.base_manifest {
            for source in &base.sources {
                sources.insert(source.observation().source().clone(), source.clone());
            }
            for removal in &base.removals {
                removals.insert(removal.source().clone(), removal.clone());
            }
            for missing in base.source_catalog().missing_sources() {
                missing_sources.insert(missing.source().clone(), missing.clone());
            }
        }
        for (source, removal) in &self.deletions {
            sources.remove(source);
            removals.insert(source.clone(), removal.clone());
            missing_sources.remove(source);
        }
        for pending in self.pending.values() {
            let certificate = pending.certificate.as_ref().ok_or_else(|| {
                IndexError::SourceNotCertified(pending.source.identity().to_string())
            })?;
            sources.insert(pending.source.clone(), certificate.clone());
            removals.remove(&pending.source);
            missing_sources.remove(&pending.source);
        }
        for (source, missing) in &self.observed_missing {
            missing_sources.insert(source.clone(), missing.clone());
        }
        let sources = sources.into_values().collect::<Vec<_>>();
        let record_aggregates = staging::manifest_record_aggregates(self, &sources)?;
        GenerationManifest::from_catalog_parts_with_record_aggregates(
            sources,
            record_aggregates,
            removals.into_values().collect(),
            SourceCatalogCheckpoint::from_missing_sources(missing_sources.into_values().collect())?,
        )
    }
}
