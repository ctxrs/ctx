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
        self.source_staging_events(event_ids)
    }

    /// Retains one source-local Flat view across every bounded Core page.
    pub(in crate::semantic) fn begin_reconciliation_view(&self, id: &str) -> FlatResult<()> {
        self.begin_reconciliation_view_inner(id, None)
    }

    pub(in crate::semantic) fn begin_source_reconciliation_view(
        &self,
        source_identity_digest: &str,
        source_reconciliation_id: &str,
        baseline_publication: &FlatPublicationToken,
        expected_page: Option<&FlatSourceStagingToken>,
    ) -> FlatResult<FlatSourceStageResume> {
        self.begin_source_staging(
            source_identity_digest,
            source_reconciliation_id,
            baseline_publication,
            expected_page,
        )
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
        self.finish_source_staging(receipt_input.as_ref())
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

    pub(in crate::semantic) fn source_receipts_match_active_authority(&self) -> FlatResult<bool> {
        let _guard = self.lock_shared()?;
        let Some(selected) = select_manifest(&self.root, &self.contract)? else {
            return Ok(false);
        };
        let manifest = &selected.envelope.manifest;
        let mut receipts = manifest
            .source_snapshots
            .iter()
            .map(|snapshot| {
                let receipt = snapshot.receipt.as_ref().ok_or_else(|| {
                    FlatStoreError::Corrupt(
                        "source authority validation found an incomplete receipt".to_owned(),
                    )
                })?;
                let mut digest = Sha256::new();
                digest.update(FLAT_SOURCE_RECEIPT_DOMAIN);
                Ok((
                    snapshot.source_identity_digest.clone(),
                    (receipt, digest, 0_u64),
                ))
            })
            .collect::<FlatResult<HashMap<_, _>>>()?;
        let (events, _) = load_active_events(&self.root, &self.contract, manifest, None)?;
        for event in events.iter() {
            let Some((receipt, digest, count)) = receipts.get_mut(&event.source_identity_digest)
            else {
                return Ok(false);
            };
            if event.source_reconciliation_id != receipt.source_reconciliation_id {
                return Ok(false);
            }
            digest.update(event.event_id.as_bytes());
            digest.update([0]);
            digest.update(event.seq.to_be_bytes());
            digest.update(event.source_text_hash.as_bytes());
            digest.update(event.stable_identity_hash);
            digest.update([0]);
            *count = count.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt("source receipt event count overflow".to_owned())
            })?;
        }
        Ok(receipts.into_values().all(|(receipt, digest, count)| {
            count == receipt.owned_event_count
                && encode_hex(&digest.finalize()) == receipt.owned_event_ids_hash
        }))
    }

    pub(in crate::semantic) fn reconciliation_active(&self) -> FlatResult<bool> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        if view.is_some() {
            return Ok(true);
        }
        drop(view);
        let stage = self.source_stage.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
        })?;
        Ok(stage.is_some())
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
    /// Appends one bounded source page to the private durable staging log.
    pub(in crate::semantic) fn publish_source_event_page(
        &self,
        replacements: &[FlatEventReplacement],
        authority_updates: &[FlatEventMetadataUpdate],
        tombstones: &[Uuid],
        existing: &FlatActiveEventLookup,
    ) -> FlatResult<FlatSourcePageOutcome> {
        self.require_writable()?;
        self.stage_source_event_page(replacements, authority_updates, tombstones, existing)
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
