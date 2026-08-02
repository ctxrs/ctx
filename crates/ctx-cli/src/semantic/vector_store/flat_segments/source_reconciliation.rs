use super::*;

impl FlatSegmentStore {
    pub(in crate::semantic) fn active_event_lookup(&self) -> FlatResult<FlatActiveEventLookup> {
        if let Some(lookup) = self.reconciliation_lookup()? {
            return Ok(lookup);
        }
        let _guard = self.lock_shared()?;
        let events = match select_manifest(&self.root, &self.contract)? {
            Some(selected) => {
                let (events, touched) = load_active_events(
                    &self.root,
                    &self.contract,
                    &selected.envelope.manifest,
                    None,
                )?;
                self.touch_metadata(touched);
                events
            }
            None => Arc::new(Vec::new()),
        };
        self.record_active_event_snapshot();
        Ok(FlatActiveEventLookup { events })
    }

    pub(in crate::semantic) fn source_reconciliation_events(
        &self,
        event_ids: &[Uuid],
    ) -> FlatResult<Vec<Option<FlatActiveEvent>>> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        let view = view.as_ref().ok_or_else(|| {
            FlatStoreError::InvalidInput(
                "source event lookup has no retained reconciliation view".to_owned(),
            )
        })?;
        if view.source.is_none() {
            return Err(FlatStoreError::InvalidInput(
                "source event lookup has an unscoped reconciliation view".to_owned(),
            ));
        }
        Ok(event_ids
            .iter()
            .map(|event_id| view.event(*event_id).cloned())
            .collect())
    }

    /// Retains one source-local Flat view across every bounded Core page.
    pub(in crate::semantic) fn begin_reconciliation_view(&self, id: &str) -> FlatResult<()> {
        self.begin_reconciliation_view_inner(id, None)
    }

    pub(in crate::semantic) fn begin_source_reconciliation_view(
        &self,
        source_identity_digest: &str,
        source_reconciliation_id: &str,
    ) -> FlatResult<()> {
        let source = FlatSourceScope {
            source_identity_digest: source_identity_digest.to_owned(),
            source_reconciliation_id: source_reconciliation_id.to_owned(),
        };
        self.begin_reconciliation_view_inner(source_reconciliation_id, Some(source))
    }

    fn begin_reconciliation_view_inner(
        &self,
        id: &str,
        source: Option<FlatSourceScope>,
    ) -> FlatResult<()> {
        if id.is_empty() {
            return Err(FlatStoreError::InvalidInput(
                "flat reconciliation view id cannot be empty".to_owned(),
            ));
        }
        let replace_existing = {
            let view = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            match view.as_ref() {
                Some(view) if view.id == id && view.source == source => return Ok(()),
                Some(_) => true,
                None => false,
            }
        };
        if replace_existing {
            self.finish_reconciliation_view()?;
        }

        let _guard = self.lock_shared()?;
        #[cfg(test)]
        let source_catalog = source.is_some();
        let events = match select_manifest(&self.root, &self.contract)? {
            Some(selected) => {
                let (events, touched) = load_active_events(
                    &self.root,
                    &self.contract,
                    &selected.envelope.manifest,
                    source
                        .as_ref()
                        .map(|scope| scope.source_identity_digest.as_str()),
                )?;
                self.touch_metadata(touched);
                #[cfg(test)]
                if source_catalog {
                    self.source_catalog_records_replayed
                        .fetch_add(touched, Ordering::Relaxed);
                }
                events
            }
            None => Arc::new(Vec::new()),
        };
        self.record_active_event_snapshot();
        #[cfg(test)]
        if source_catalog {
            self.source_catalog_load_count
                .fetch_add(1, Ordering::Relaxed);
        }
        let mut view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        *view = Some(FlatReconciliationView {
            id: id.to_owned(),
            source,
            lookup: FlatActiveEventLookup { events },
            updates: HashMap::new(),
            after_event_id: None,
            pending_event_page: None,
            retirement_event_ids: None,
        });
        Ok(())
    }

    pub(in crate::semantic) fn reconciliation_event_ids(
        &self,
        id: &str,
        limit: usize,
    ) -> FlatResult<Vec<Uuid>> {
        if limit == 0 {
            return Err(FlatStoreError::InvalidInput(
                "flat reconciliation event page limit cannot be zero".to_owned(),
            ));
        }
        let mut current = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        let view = current
            .as_mut()
            .filter(|view| view.id == id)
            .ok_or_else(|| {
                FlatStoreError::InvalidInput(
                    "flat reconciliation event page has no matching view".to_owned(),
                )
            })?;
        if let Some(pending) = view.pending_event_page.as_ref() {
            return Ok(pending.event_ids.clone());
        }
        if view.retirement_event_ids.is_none() {
            let source_reconciliation_id = view
                .source
                .as_ref()
                .map(|source| source.source_reconciliation_id.as_str());
            view.retirement_event_ids = Some(
                view.current_events()
                    .into_iter()
                    .filter(|event| {
                        source_reconciliation_id.is_none_or(|reconciliation_id| {
                            event.source_reconciliation_id != reconciliation_id
                        })
                    })
                    .map(|event| event.event_id)
                    .collect(),
            );
        }
        let events = view.retirement_event_ids.as_deref().unwrap_or_default();
        let start = view.after_event_id.map_or(0, |after| {
            events.partition_point(|event_id| *event_id <= after)
        });
        let event_ids = events[start..]
            .iter()
            .take(limit)
            .copied()
            .collect::<Vec<_>>();
        if let Some(after_event_id) = event_ids.last().copied() {
            view.pending_event_page = Some(FlatReconciliationEventPage {
                event_ids: event_ids.clone(),
                after_event_id,
            });
        }
        Ok(event_ids)
    }

    pub(in crate::semantic) fn finish_reconciliation_view(&self) -> FlatResult<()> {
        let retained = {
            let mut current = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            current.take()
        };
        let Some(retained) = retained else {
            return Ok(());
        };
        if retained.source.is_some() {
            return Ok(());
        }
        if let Err(error) = self.compact().map(|_| ()) {
            let mut current = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            if current.is_none() {
                *current = Some(retained);
            }
            return Err(error);
        }
        Ok(())
    }

    pub(in crate::semantic) fn finish_source_reconciliation_view(
        &self,
        receipt_input: Option<FlatSourceReceiptInput>,
    ) -> FlatResult<FlatSourceFinalization> {
        let retained = {
            let mut current = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            current.take().ok_or_else(|| {
                FlatStoreError::InvalidInput(
                    "source finalization has no retained reconciliation view".to_owned(),
                )
            })?
        };
        let finish = self.publish_source_finalization(&retained, receipt_input.as_ref());
        if finish.is_err() {
            let mut current = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            if current.is_none() {
                *current = Some(retained);
            }
        }
        finish
    }

    pub(in crate::semantic) fn compact_if_needed(&self) -> FlatResult<()> {
        let stats = self.active_stats()?;
        if stats.segment_count >= COMPACT_SEGMENT_THRESHOLD
            || (stats.active_chunks > 0
                && stats.stored_chunks > (stats.active_chunks as u64).saturating_mul(2))
        {
            let _ = self.compact()?;
        }
        Ok(())
    }

    pub(in crate::semantic) fn reconciliation_active(&self) -> FlatResult<bool> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view.is_some())
    }

    fn reconciliation_lookup(&self) -> FlatResult<Option<FlatActiveEventLookup>> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view.as_ref().map(|view| FlatActiveEventLookup {
            events: Arc::new(view.current_events()),
        }))
    }

    pub(super) fn full_reconciliation_lookup(&self) -> FlatResult<Option<FlatActiveEventLookup>> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view
            .as_ref()
            .filter(|view| view.source.is_none())
            .map(|view| FlatActiveEventLookup {
                events: Arc::new(view.current_events()),
            }))
    }
}

impl FlatSegmentStore {
    /// Publishes vector changes, reused-vector authority updates, and
    /// retirements for one bounded source page in one Flat generation.
    pub(in crate::semantic) fn publish_source_event_page(
        &self,
        replacements: &[FlatEventReplacement],
        authority_updates: &[FlatEventMetadataUpdate],
        tombstones: &[Uuid],
    ) -> FlatResult<FlatPublishOutcome> {
        self.require_writable()?;
        validate_publication_input(&self.contract, replacements, tombstones)?;
        let _guard = self.lock_exclusive()?;
        let current = self.load_current_locked()?;
        if replacements.is_empty() && authority_updates.is_empty() && tombstones.is_empty() {
            return Ok(noop_outcome(current.as_ref()));
        }
        let (source, existing) = {
            let view = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            let view = view.as_ref().ok_or_else(|| {
                FlatStoreError::InvalidInput(
                    "source page publication has no retained reconciliation view".to_owned(),
                )
            })?;
            let source = view.source.clone().ok_or_else(|| {
                FlatStoreError::InvalidInput(
                    "source page publication has an unscoped reconciliation view".to_owned(),
                )
            })?;
            (
                source,
                view.touched_lookup(replacements, authority_updates, tombstones),
            )
        };
        let generation = next_generation(current.as_ref())?;
        let staged = write_event_segment(
            &self.root,
            &self.contract,
            generation,
            &source,
            EventSegmentInput {
                replacements,
                authority_updates,
                tombstones,
                existing: &existing,
            },
        )?;
        sync_directory(&segments_directory(&self.root))?;
        validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;

        let mut manifest = current
            .as_ref()
            .map(|selected| selected.envelope.manifest.clone())
            .unwrap_or_else(|| Manifest::new(self.contract.clone()));
        manifest.generation = generation;
        manifest.created_unix_millis = unix_millis();
        apply_publication_counts(&mut manifest, &existing, replacements, tombstones)?;
        if source.source_identity_digest != UNSCOPED_SOURCE_IDENTITY {
            manifest.segments.retain(|segment| {
                segment.source_identity_digest != UNSCOPED_SOURCE_IDENTITY
                    || segment.vector_count != 0
                    || segment.mutation_count != 0
            });
        }
        manifest.segments.push(staged.descriptor.clone());
        let selected = publish_manifest(&self.root, manifest)?;
        #[cfg(test)]
        self.source_publication_count
            .fetch_add(1, Ordering::Relaxed);
        self.record_reconciliation_publication(&staged)?;
        self.clear_pinned()?;
        self.touch_vectors(replacements)?;
        self.touch_metadata(u64::try_from(staged.mutations.len()).unwrap_or(u64::MAX));
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatPublishOutcome {
            published: true,
            generation,
            generation_hash: Some(selected.generation_hash),
            replaced_events: authority_updates.len(),
            deleted_events: tombstones.len(),
        })
    }

    pub(super) fn current_source_scope(&self) -> FlatResult<FlatSourceScope> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view
            .as_ref()
            .and_then(|view| view.source.clone())
            .unwrap_or_else(unscoped_source))
    }

    pub(super) fn load_source_events(
        &self,
        current: Option<&SelectedManifest>,
        source: &FlatSourceScope,
    ) -> FlatResult<(FlatActiveEventLookup, u64)> {
        let Some(current) = current else {
            return Ok((
                FlatActiveEventLookup {
                    events: Arc::new(Vec::new()),
                },
                0,
            ));
        };
        let (events, touched) = load_active_events(
            &self.root,
            &self.contract,
            &current.envelope.manifest,
            Some(&source.source_identity_digest),
        )?;
        Ok((FlatActiveEventLookup { events }, touched))
    }

    fn publish_source_finalization(
        &self,
        view: &FlatReconciliationView,
        receipt_input: Option<&FlatSourceReceiptInput>,
    ) -> FlatResult<FlatSourceFinalization> {
        self.require_writable()?;
        let source = view.source.as_ref().ok_or_else(|| {
            FlatStoreError::InvalidInput(
                "source finalization has an unscoped reconciliation view".to_owned(),
            )
        })?;
        let events = view.current_events();
        if events.iter().any(|event| {
            event.source_identity_digest != source.source_identity_digest
                || event.source_reconciliation_id != source.source_reconciliation_id
        }) {
            return Err(FlatStoreError::Corrupt(
                "source finalization retained stale event authority".to_owned(),
            ));
        }
        let receipt = receipt_input
            .map(|input| build_flat_source_receipt(input, &events))
            .transpose()?;
        if receipt.as_ref().is_some_and(|receipt| {
            receipt.source_identity_digest != source.source_identity_digest
                || receipt.source_reconciliation_id != source.source_reconciliation_id
        }) {
            return Err(FlatStoreError::InvalidInput(
                "source receipt does not match its reconciliation view".to_owned(),
            ));
        }

        let _guard = self.lock_exclusive()?;
        let current = self.load_current_locked()?;
        let generation = next_generation(current.as_ref())?;
        let current_manifest = current.as_ref().map(|selected| &selected.envelope.manifest);
        let source_segments = current_manifest
            .into_iter()
            .flat_map(|manifest| &manifest.segments)
            .filter(|segment| segment.source_identity_digest == source.source_identity_digest)
            .collect::<Vec<_>>();
        let active_chunks = events.iter().try_fold(0_u64, |total, event| {
            total
                .checked_add(u64::from(event.chunk_count))
                .ok_or_else(|| FlatStoreError::Corrupt("source chunk count overflow".to_owned()))
        })?;
        let stored_chunks = source_segments.iter().try_fold(0_u64, |total, segment| {
            total
                .checked_add(segment.vector_count)
                .ok_or_else(|| FlatStoreError::Corrupt("stored source chunk overflow".to_owned()))
        })?;
        let compact = receipt.is_some()
            && (source_segments.len() >= COMPACT_SEGMENT_THRESHOLD
                || (active_chunks > 0 && stored_chunks > active_chunks.saturating_mul(2)));
        let snapshot_source = if receipt.is_some() {
            source.clone()
        } else {
            unscoped_source()
        };
        let staged = if compact {
            write_source_compacted_segment(
                &self.root,
                &self.contract,
                generation,
                source,
                &events,
                current_manifest.ok_or_else(|| {
                    FlatStoreError::Corrupt("source compaction has no current manifest".to_owned())
                })?,
            )?
        } else {
            let mutations = events.iter().map(event_mutation).collect::<Vec<_>>();
            write_catalog_segment(
                &self.root,
                &self.contract,
                generation,
                &snapshot_source,
                SegmentKind::Base,
                &mutations,
            )?
        };
        sync_directory(&segments_directory(&self.root))?;
        validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;

        let mut manifest = current
            .as_ref()
            .map(|selected| selected.envelope.manifest.clone())
            .unwrap_or_else(|| Manifest::new(self.contract.clone()));
        manifest.generation = generation;
        manifest.created_unix_millis = unix_millis();
        if compact || receipt.is_none() {
            manifest.segments.retain(|segment| {
                segment.source_identity_digest != source.source_identity_digest
                    && (receipt.is_some()
                        || segment.source_identity_digest != UNSCOPED_SOURCE_IDENTITY
                        || segment.vector_count != 0
                        || segment.mutation_count != 0)
            });
        } else {
            manifest.segments.retain(|segment| {
                segment.source_identity_digest != source.source_identity_digest
                    || segment.vector_count != 0
            });
        }
        manifest.segments.push(staged.descriptor);
        match receipt.as_ref() {
            Some(receipt) => set_source_snapshot_receipt(
                &mut manifest,
                &source.source_identity_digest,
                generation,
                receipt.clone(),
            ),
            None => remove_source_snapshot(&mut manifest, &source.source_identity_digest),
        }
        let selected = publish_manifest(&self.root, manifest)?;
        #[cfg(test)]
        self.source_publication_count
            .fetch_add(1, Ordering::Relaxed);
        self.clear_pinned()?;
        if compact {
            self.record_compaction_work(active_chunks, events.len())?;
        } else {
            self.touch_metadata(u64::try_from(events.len()).unwrap_or(u64::MAX));
        }
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatSourceFinalization {
            publication: FlatPublishOutcome {
                published: true,
                generation,
                generation_hash: Some(selected.generation_hash),
                replaced_events: events.len(),
                deleted_events: 0,
            },
            receipt,
        })
    }

    pub(super) fn record_reconciliation_publication(
        &self,
        staged: &StagedSegment,
    ) -> FlatResult<()> {
        let mut current = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        let Some(view) = current.as_mut() else {
            return Ok(());
        };
        let tombstones = staged
            .mutations
            .iter()
            .filter(|mutation| mutation.kind == MutationKind::Delete)
            .map(|mutation| mutation.event_id)
            .collect::<HashSet<_>>();
        if view.pending_event_page.as_ref().is_some_and(|pending| {
            pending.event_ids.len() == tombstones.len()
                && pending
                    .event_ids
                    .iter()
                    .all(|event_id| tombstones.contains(event_id))
        }) {
            let pending = view.pending_event_page.take().ok_or_else(|| {
                FlatStoreError::Corrupt("flat reconciliation event page was lost".to_owned())
            })?;
            view.after_event_id = Some(pending.after_event_id);
        }
        view.apply_publication(staged);
        Ok(())
    }
}

fn build_flat_source_receipt(
    input: &FlatSourceReceiptInput,
    events: &[FlatActiveEvent],
) -> FlatResult<FlatSourceReceipt> {
    for (value, field) in [
        (&input.source_identity_digest, "source identity digest"),
        (&input.core_record_accumulator, "Core record accumulator"),
        (&input.contract_fingerprint, "source contract fingerprint"),
        (
            &input.semantic_policy_fingerprint,
            "semantic policy fingerprint",
        ),
    ] {
        if decode_sha256(value).is_none() {
            return Err(FlatStoreError::InvalidInput(format!(
                "{field} must be lowercase SHA-256"
            )));
        }
    }
    let owned_event_count = u64::try_from(events.len()).map_err(|_| {
        FlatStoreError::InvalidInput("source receipt event count is too large".to_owned())
    })?;
    if input.source_reconciliation_id.is_empty()
        || owned_event_count > input.semantic_eligible_documents
    {
        return Err(FlatStoreError::InvalidInput(
            "source receipt exceeds or does not identify its Core aggregate".to_owned(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(FLAT_SOURCE_RECEIPT_DOMAIN);
    for event in events {
        digest.update(event.event_id.as_bytes());
        digest.update([0]);
        digest.update(event.seq.to_be_bytes());
        digest.update(event.source_text_hash.as_bytes());
        digest.update(event.stable_identity_hash);
        digest.update([0]);
    }
    Ok(FlatSourceReceipt {
        source_identity_digest: input.source_identity_digest.clone(),
        source_reconciliation_id: input.source_reconciliation_id.clone(),
        indexed_documents: input.indexed_documents,
        semantic_eligible_documents: input.semantic_eligible_documents,
        core_record_accumulator: input.core_record_accumulator.clone(),
        contract_fingerprint: input.contract_fingerprint.clone(),
        semantic_policy_fingerprint: input.semantic_policy_fingerprint.clone(),
        owned_event_count,
        owned_event_ids_hash: encode_hex(&digest.finalize()),
    })
}
