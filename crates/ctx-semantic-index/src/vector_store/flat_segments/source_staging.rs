use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
};

use super::*;

pub(super) struct FlatSourceStage {
    source: FlatSourceScope,
    baseline_publication: FlatPublicationToken,
    directory: PathBuf,
    pages: Vec<SourceStagePage>,
    baseline_cursor: Option<SourceBaselineCursor>,
    finalized: Option<SourceStageFinal>,
}

struct SourceBaselineCursor {
    cursors: Vec<SourceMutationCursor>,
    heap: BinaryHeap<Reverse<(Uuid, usize)>>,
    vectors: HashMap<u64, u64>,
    pending: Option<FlatActiveEvent>,
    active_events: u64,
    active_chunks: u64,
    source: FlatSourceScope,
}

struct SourceMutationCursor {
    descriptor: SegmentDescriptor,
    mapping: Mmap,
    next_ordinal: usize,
    record_count: usize,
    previous_event_id: Option<Uuid>,
    current: EventMutation,
}

impl FlatSegmentStore {
    pub(crate) fn begin_source_staging(
        &self,
        source_identity_digest: &str,
        source_reconciliation_id: &str,
        baseline_publication: &FlatPublicationToken,
        expected_page: Option<&FlatSourceStagingToken>,
    ) -> FlatResult<FlatSourceStageResume> {
        let source = FlatSourceScope {
            source_identity_digest: source_identity_digest.to_owned(),
            source_reconciliation_id: source_reconciliation_id.to_owned(),
        };
        {
            let stage = self.source_stage.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
            })?;
            if stage.as_ref().is_some_and(|stage| {
                stage.source == source
                    && &stage.baseline_publication == baseline_publication
                    && stage.page_token().as_ref() == expected_page
            }) {
                return Ok(FlatSourceStageResume::Ready);
            }
        }

        let (active_publication, manifest) = self.source_generation_manifest()?;
        match publication_order(baseline_publication, &active_publication)? {
            std::cmp::Ordering::Greater => {
                return Err(FlatStoreError::Corrupt(
                    "active Flat publication predates its source baseline".to_owned(),
                ));
            }
            std::cmp::Ordering::Less => {
                let finalization = read_source_stage_final(&source_stage_directory(&self.root))?
                    .ok_or_else(|| {
                        FlatStoreError::Corrupt(
                            "active Flat candidate has no retained source finalization".to_owned(),
                        )
                    })?;
                validate_retained_candidate(
                    &finalization,
                    &source,
                    baseline_publication,
                    &active_publication,
                    manifest.as_ref(),
                )?;
                let mut stage = self.source_stage.lock().map_err(|_| {
                    FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
                })?;
                *stage = Some(FlatSourceStage {
                    source,
                    baseline_publication: baseline_publication.clone(),
                    directory: source_stage_directory(&self.root),
                    pages: Vec::new(),
                    baseline_cursor: None,
                    finalized: Some(finalization),
                });
                return Ok(FlatSourceStageResume::Ready);
            }
            std::cmp::Ordering::Equal => {}
        }

        let directory = source_stage_directory(&self.root);
        let pages = if let Some(expected_page) = expected_page {
            match load_source_stage(
                &directory,
                &self.contract,
                &source,
                baseline_publication,
                expected_page,
            ) {
                Ok(pages) => pages,
                Err(_) => {
                    reset_source_stage_directory(&directory)?;
                    write_stage_baseline(&directory, &source, baseline_publication)?;
                    let cursor = SourceBaselineCursor::new(
                        &self.root,
                        &self.contract,
                        manifest.as_ref(),
                        source.clone(),
                    )?;
                    let mut stage = self.source_stage.lock().map_err(|_| {
                        FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
                    })?;
                    *stage = Some(FlatSourceStage {
                        source,
                        baseline_publication: baseline_publication.clone(),
                        directory,
                        pages: Vec::new(),
                        baseline_cursor: Some(cursor),
                        finalized: None,
                    });
                    return Ok(FlatSourceStageResume::Restarted);
                }
            }
        } else {
            reset_source_stage_directory(&directory)?;
            write_stage_baseline(&directory, &source, baseline_publication)?;
            Vec::new()
        };
        let cursor = SourceBaselineCursor::new(
            &self.root,
            &self.contract,
            manifest.as_ref(),
            source.clone(),
        )?;
        let mut stage = self.source_stage.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
        })?;
        *stage = Some(FlatSourceStage {
            source,
            baseline_publication: baseline_publication.clone(),
            directory,
            pages,
            baseline_cursor: Some(cursor),
            finalized: None,
        });
        Ok(FlatSourceStageResume::Ready)
    }

    pub(crate) fn source_staging_events(
        &self,
        event_ids: &[Uuid],
    ) -> FlatResult<Vec<Option<FlatActiveEvent>>> {
        let mut stage = self.source_stage.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
        })?;
        let stage = stage.as_mut().ok_or_else(|| {
            FlatStoreError::InvalidInput("source event lookup has no active staging log".to_owned())
        })?;
        let cursor = stage.baseline_cursor.as_mut().ok_or_else(|| {
            FlatStoreError::InvalidInput(
                "finalized source staging log has no baseline cursor".to_owned(),
            )
        })?;
        let events = cursor.lookup(event_ids)?;
        #[cfg(test)]
        self.staging_peak_event_records.fetch_max(
            u64::try_from(events.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        Ok(events)
    }

    pub(crate) fn stage_source_event_page(
        &self,
        replacements: &[FlatEventReplacement],
        authority_updates: &[FlatEventMetadataUpdate],
        tombstones: &[Uuid],
        existing: &FlatActiveEventLookup,
    ) -> FlatResult<FlatSourcePageOutcome> {
        validate_publication_input(&self.contract, replacements, tombstones)?;
        let mut stage = self.source_stage.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
        })?;
        let stage = stage.as_mut().ok_or_else(|| {
            FlatStoreError::InvalidInput("source page has no active staging log".to_owned())
        })?;
        if stage.finalized.is_some() {
            return Err(FlatStoreError::InvalidInput(
                "cannot append a page to finalized source staging".to_owned(),
            ));
        }
        let next_sequence = u64::try_from(stage.pages.len())
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| FlatStoreError::Corrupt("source stage sequence overflow".to_owned()))?;
        let generation = stage
            .baseline_publication
            .generation
            .checked_add(next_sequence)
            .ok_or_else(|| {
                FlatStoreError::Corrupt("source stage generation overflow".to_owned())
            })?;
        let staged =
            if replacements.is_empty() && authority_updates.is_empty() && tombstones.is_empty() {
                None
            } else {
                let staged = write_event_segment_in_directory(
                    &stage.directory,
                    &self.contract,
                    generation,
                    &stage.source,
                    EventSegmentInput {
                        replacements,
                        authority_updates,
                        tombstones,
                        existing,
                    },
                )?;
                sync_directory(&stage.directory)?;
                validate_staged_segment_in_directory(
                    &stage.directory,
                    &self.contract,
                    &staged.descriptor,
                )?;
                Some(staged)
            };
        let (active_events, active_chunks) = staged
            .as_ref()
            .map(|staged| {
                staged
                    .mutations
                    .iter()
                    .filter(|mutation| mutation.kind == MutationKind::Replace)
                    .try_fold(
                        (0_u64, 0_u64),
                        |(events, chunks), mutation| -> FlatResult<_> {
                            Ok((
                                events.checked_add(1).ok_or_else(|| {
                                    FlatStoreError::Corrupt(
                                        "staged source event overflow".to_owned(),
                                    )
                                })?,
                                chunks
                                    .checked_add(u64::from(mutation.chunk_count))
                                    .ok_or_else(|| {
                                        FlatStoreError::Corrupt(
                                            "staged source chunk overflow".to_owned(),
                                        )
                                    })?,
                            ))
                        },
                    )
            })
            .transpose()?
            .unwrap_or_default();
        let replacement_ids = replacements
            .iter()
            .map(|replacement| replacement.event_id)
            .collect::<std::collections::HashSet<_>>();
        let reused_chunks = authority_updates.iter().try_fold(0_u64, |total, update| {
            if replacement_ids.contains(&update.event_id) {
                return Ok(total);
            }
            total
                .checked_add(
                    existing
                        .event(update.event_id)
                        .map_or(0, |event| u64::from(event.chunk_count)),
                )
                .ok_or_else(|| FlatStoreError::Corrupt("reused chunk count overflow".to_owned()))
        })?;
        let previous_page_hash = stage.pages.last().map(stage_page_hash).transpose()?;
        let page = SourceStagePage {
            schema_version: SOURCE_STAGE_SCHEMA_VERSION,
            source: stage.source.clone(),
            page_sequence: next_sequence,
            previous_page_hash,
            descriptor: staged.as_ref().map(|staged| staged.descriptor.clone()),
            active_events,
            active_chunks,
            reused_chunks,
        };
        let page_hash = write_stage_page(&stage.directory, &page)?;
        stage.pages.push(page);
        self.touch_vectors(replacements)?;
        self.touch_metadata(
            u64::try_from(
                replacements
                    .len()
                    .saturating_add(authority_updates.len())
                    .saturating_add(tombstones.len()),
            )
            .unwrap_or(u64::MAX),
        );
        #[cfg(test)]
        self.staging_peak_event_records.fetch_max(
            u64::try_from(existing.events.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        Ok(FlatSourcePageOutcome {
            staging: FlatSourceStagingToken {
                source_reconciliation_id: stage.source.source_reconciliation_id.clone(),
                page_sequence: next_sequence,
                page_hash,
            },
        })
    }

    pub(crate) fn finish_source_staging(
        &self,
        receipt_input: Option<&FlatSourceReceiptInput>,
    ) -> FlatResult<FlatSourceFinalization> {
        let mut stage = self.source_stage.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
        })?;
        let stage = stage.as_mut().ok_or_else(|| {
            FlatStoreError::InvalidInput("source finalization has no staging log".to_owned())
        })?;
        if let Some(finalized) = stage.finalized.as_ref() {
            return finalized.outcome();
        }
        let (active_publication, current_manifest) = self.source_generation_manifest()?;
        if publication_order(&stage.baseline_publication, &active_publication)?
            != std::cmp::Ordering::Equal
        {
            return Err(FlatStoreError::Corrupt(
                "Flat source baseline changed before final publication".to_owned(),
            ));
        }
        let cursor = stage.baseline_cursor.as_mut().ok_or_else(|| {
            FlatStoreError::Corrupt("source staging lost its baseline cursor".to_owned())
        })?;
        cursor.drain()?;
        let old_events = cursor.active_events;
        let old_chunks = cursor.active_chunks;
        let final_generation = stage
            .baseline_publication
            .generation
            .checked_add(u64::try_from(stage.pages.len()).unwrap_or(u64::MAX))
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| {
                FlatStoreError::Corrupt("source final generation overflow".to_owned())
            })?;
        let prior_final = read_source_stage_final(&stage.directory)?;
        let created_unix_millis = prior_final
            .as_ref()
            .map_or_else(unix_millis, |finalized| finalized.created_unix_millis);
        let (catalog, receipt, new_events, new_chunks) = write_final_source_catalog(
            &stage.directory,
            &self.contract,
            final_generation,
            &stage.source,
            &stage.pages,
            receipt_input,
        )?;
        let (published_source, compacted) = if receipt.is_some() {
            compact_staged_source_if_needed(
                StagedSourceCompaction {
                    root: &self.root,
                    staging: &stage.directory,
                    contract: &self.contract,
                    generation: final_generation,
                    source: &stage.source,
                    current: current_manifest
                        .as_ref()
                        .map(|selected| &selected.envelope.manifest),
                    pages: &stage.pages,
                    active_chunks: new_chunks,
                },
                catalog,
            )?
        } else {
            (catalog, false)
        };
        link_staged_descriptor(
            &stage.directory,
            &segments_directory(&self.root),
            &published_source.descriptor,
        )?;
        let mut manifest = current_manifest
            .as_ref()
            .map(|selected| selected.envelope.manifest.clone())
            .unwrap_or_else(|| Manifest::new(self.contract.clone()));
        manifest.generation = final_generation;
        manifest.created_unix_millis = created_unix_millis;
        manifest.active_events = manifest
            .active_events
            .checked_sub(old_events)
            .and_then(|value| value.checked_add(new_events))
            .ok_or_else(|| {
                FlatStoreError::Corrupt("source active event count overflow".to_owned())
            })?;
        manifest.active_chunks = manifest
            .active_chunks
            .checked_sub(old_chunks)
            .and_then(|value| value.checked_add(new_chunks))
            .ok_or_else(|| {
                FlatStoreError::Corrupt("source active chunk count overflow".to_owned())
            })?;
        if receipt.is_some() {
            if compacted {
                manifest.segments.retain(|descriptor| {
                    descriptor.source_identity_digest != stage.source.source_identity_digest
                });
            } else {
                manifest.segments.retain(|descriptor| {
                    descriptor.source_identity_digest != stage.source.source_identity_digest
                        || descriptor.vector_count != 0
                });
                for page in &stage.pages {
                    if let Some(descriptor) = page
                        .descriptor
                        .as_ref()
                        .filter(|descriptor| descriptor.vector_count != 0)
                    {
                        link_staged_descriptor(
                            &stage.directory,
                            &segments_directory(&self.root),
                            descriptor,
                        )?;
                        manifest.segments.push(descriptor.clone());
                    }
                }
            }
            manifest.segments.push(published_source.descriptor.clone());
            set_source_snapshot_receipt(
                &mut manifest,
                &stage.source.source_identity_digest,
                final_generation,
                receipt.clone().ok_or_else(|| {
                    FlatStoreError::Corrupt("source receipt disappeared".to_owned())
                })?,
            );
        } else {
            manifest.segments.retain(|descriptor| {
                descriptor.source_identity_digest != stage.source.source_identity_digest
            });
            manifest.segments.push(published_source.descriptor.clone());
            remove_source_snapshot(&mut manifest, &stage.source.source_identity_digest);
        }
        sync_directory(&segments_directory(&self.root))?;
        let prepared = prepare_manifest(manifest)?;
        #[cfg(test)]
        self.global_manifest_serialization_count
            .fetch_add(1, Ordering::Relaxed);
        let candidate_publication = FlatPublicationToken {
            generation: prepared.envelope.manifest.generation,
            generation_hash: Some(prepared.generation_hash.clone()),
        };
        let finalized = SourceStageFinal {
            schema_version: SOURCE_STAGE_SCHEMA_VERSION,
            source: stage.source.clone(),
            baseline_publication: stage.baseline_publication.clone(),
            final_page: stage.page_token(),
            candidate_publication: candidate_publication.clone(),
            created_unix_millis,
            active_events: new_events,
            active_chunks: new_chunks,
            deleted_chunks: old_chunks.saturating_sub(
                stage.pages.iter().fold(0_u64, |total, page| {
                    total.saturating_add(page.reused_chunks)
                }),
            ),
            receipt: receipt.clone(),
            catalog: published_source.descriptor,
        };
        if let Some(prior) = prior_final {
            if prior != finalized {
                return Err(FlatStoreError::Corrupt(
                    "retained Flat source candidate disagrees with deterministic replay".to_owned(),
                ));
            }
        } else {
            write_source_stage_final(&stage.directory, &finalized)?;
        }
        let selected = {
            let _guard = self.lock_exclusive()?;
            publish_prepared_manifest(&self.root, prepared)?
        };
        if selected.generation_hash
            != candidate_publication
                .generation_hash
                .as_deref()
                .unwrap_or_default()
        {
            return Err(FlatStoreError::Corrupt(
                "published Flat source candidate changed hash".to_owned(),
            ));
        }
        {
            let mut generation = self.source_generation.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat source generation lock is poisoned".to_owned())
            })?;
            let generation = generation.as_mut().ok_or_else(|| {
                FlatStoreError::Corrupt("source finalization lost its generation view".to_owned())
            })?;
            generation.selected = Some(selected.clone());
        }
        self.clear_pinned()?;
        if compacted {
            self.record_compaction_work(
                new_chunks,
                usize::try_from(new_events).map_err(|_| {
                    FlatStoreError::Corrupt("source event count does not fit usize".to_owned())
                })?,
            )?;
        }
        #[cfg(test)]
        {
            self.source_publication_count
                .fetch_add(1, Ordering::Relaxed);
            self.global_segment_directory_scan_count
                .fetch_add(1, Ordering::Relaxed);
        }
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        stage.finalized = Some(finalized.clone());
        finalized.outcome()
    }

    pub(crate) fn acknowledge_source_staging(
        &self,
        publication: &FlatPublicationToken,
    ) -> FlatResult<()> {
        self.require_writable()?;
        let in_memory = {
            let stage = self.source_stage.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
            })?;
            stage
                .as_ref()
                .map(|stage| {
                    stage.finalized.clone().ok_or_else(|| {
                        FlatStoreError::Corrupt(
                            "source acknowledgement has no finalized staging record".to_owned(),
                        )
                    })
                })
                .transpose()?
        };
        let directory = source_stage_directory(&self.root);
        let durable = read_source_stage_final(&directory)?;
        if in_memory
            .as_ref()
            .is_some_and(|finalized| durable.as_ref() != Some(finalized))
        {
            return Err(FlatStoreError::Corrupt(
                "durable source finalization disagrees with in-memory staging".to_owned(),
            ));
        }
        let Some(finalized) = durable else {
            return Ok(());
        };
        let (active, manifest) = self.source_generation_manifest()?;
        if publication != &active || finalized.candidate_publication != active {
            return Err(FlatStoreError::Corrupt(
                "source acknowledgement candidate disagrees with active Flat authority".to_owned(),
            ));
        }
        validate_retained_candidate(
            &finalized,
            &finalized.source,
            &finalized.baseline_publication,
            &active,
            manifest.as_ref(),
        )?;

        let mut stage = self.source_stage.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
        })?;
        if stage
            .as_ref()
            .is_some_and(|stage| stage.finalized.as_ref() != Some(&finalized))
        {
            return Err(FlatStoreError::Corrupt(
                "source staging changed during acknowledgement".to_owned(),
            ));
        }
        *stage = None;
        drop(stage);
        reset_source_stage_directory(&directory)
    }

    #[cfg(test)]
    pub(crate) fn fail_after_source_publication_commit_once(&self) {
        self.fail_after_source_publication_commit
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn take_source_publication_commit_failure(&self) -> bool {
        self.fail_after_source_publication_commit
            .swap(false, Ordering::Relaxed)
    }

    pub(crate) fn retained_source_candidate(
        &self,
        source_identity_digest: &str,
        baseline: &FlatPublicationToken,
        current: &FlatPublicationToken,
    ) -> FlatResult<bool> {
        let Some(finalized) = read_source_stage_final(&source_stage_directory(&self.root))? else {
            return Ok(false);
        };
        if finalized.source.source_identity_digest != source_identity_digest
            || &finalized.baseline_publication != baseline
        {
            return Ok(false);
        }
        if &finalized.candidate_publication != current {
            return Err(FlatStoreError::Corrupt(
                "retained Flat source candidate disagrees with active manifest".to_owned(),
            ));
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn corrupt_retained_source_candidate_hash(&self) -> FlatResult<()> {
        corrupt_source_stage_candidate_hash(&self.root)
    }

    fn source_generation_manifest(
        &self,
    ) -> FlatResult<(FlatPublicationToken, Option<SelectedManifest>)> {
        let generation = self.source_generation.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source generation lock is poisoned".to_owned())
        })?;
        let generation = generation.as_ref().ok_or_else(|| {
            FlatStoreError::InvalidInput("no active Flat source generation view".to_owned())
        })?;
        let publication =
            generation
                .selected
                .as_ref()
                .map_or_else(FlatPublicationToken::default, |selected| {
                    FlatPublicationToken {
                        generation: selected.envelope.manifest.generation,
                        generation_hash: Some(selected.generation_hash.clone()),
                    }
                });
        Ok((publication, generation.selected.clone()))
    }
}

impl FlatSourceStage {
    fn page_token(&self) -> Option<FlatSourceStagingToken> {
        self.pages.last().and_then(|page| {
            stage_page_hash(page)
                .ok()
                .map(|page_hash| FlatSourceStagingToken {
                    source_reconciliation_id: self.source.source_reconciliation_id.clone(),
                    page_sequence: page.page_sequence,
                    page_hash,
                })
        })
    }
}

impl SourceStageFinal {
    fn outcome(&self) -> FlatResult<FlatSourceFinalization> {
        Ok(FlatSourceFinalization {
            publication: FlatPublishOutcome {
                published: true,
                generation: self.candidate_publication.generation,
                generation_hash: self.candidate_publication.generation_hash.clone(),
                replaced_events: usize::try_from(self.active_events).map_err(|_| {
                    FlatStoreError::Corrupt("source event count does not fit usize".to_owned())
                })?,
                deleted_events: 0,
            },
            receipt: self.receipt.clone(),
            deleted_chunks: self.deleted_chunks,
        })
    }
}

impl SourceBaselineCursor {
    fn new(
        root: &Path,
        contract: &FlatModelContract,
        selected: Option<&SelectedManifest>,
        source: FlatSourceScope,
    ) -> FlatResult<Self> {
        let mut vectors = HashMap::new();
        let mut cursors = Vec::new();
        if let Some(selected) = selected {
            let manifest = &selected.envelope.manifest;
            let floor = source_snapshot_generation(manifest, &source.source_identity_digest);
            for descriptor in &manifest.segments {
                if descriptor.source_identity_digest != source.source_identity_digest {
                    continue;
                }
                if descriptor.vector_count != 0 {
                    vectors.insert(descriptor.generation, descriptor.vector_count);
                }
                if descriptor.generation < floor || descriptor.mutation_count == 0 {
                    continue;
                }
                cursors.push(SourceMutationCursor::new(root, contract, descriptor)?);
            }
        }
        let mut heap = BinaryHeap::new();
        for (index, cursor) in cursors.iter().enumerate() {
            heap.push(Reverse((cursor.current.event_id, index)));
        }
        Ok(Self {
            cursors,
            heap,
            vectors,
            pending: None,
            active_events: 0,
            active_chunks: 0,
            source,
        })
    }

    fn lookup(&mut self, event_ids: &[Uuid]) -> FlatResult<Vec<Option<FlatActiveEvent>>> {
        if event_ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(FlatStoreError::InvalidInput(
                "source staging lookup ids are not uniquely sorted".to_owned(),
            ));
        }
        let mut found = Vec::with_capacity(event_ids.len());
        for event_id in event_ids {
            loop {
                if self.pending.is_none() {
                    self.pending = self.next_active()?;
                }
                match self
                    .pending
                    .as_ref()
                    .map(|event| event.event_id.cmp(event_id))
                {
                    Some(std::cmp::Ordering::Less) => self.pending = None,
                    Some(std::cmp::Ordering::Equal) => {
                        found.push(self.pending.take());
                        break;
                    }
                    Some(std::cmp::Ordering::Greater) | None => {
                        found.push(None);
                        break;
                    }
                }
            }
        }
        Ok(found)
    }

    fn drain(&mut self) -> FlatResult<()> {
        self.pending = None;
        while self.next_active()?.is_some() {}
        Ok(())
    }

    fn next_active(&mut self) -> FlatResult<Option<FlatActiveEvent>> {
        loop {
            let Some(Reverse((event_id, _))) = self.heap.peek().copied() else {
                return Ok(None);
            };
            let mut authority = None::<(u64, EventMutation)>;
            while self
                .heap
                .peek()
                .is_some_and(|Reverse((candidate, _))| *candidate == event_id)
            {
                let Reverse((_, index)) = self.heap.pop().ok_or_else(|| {
                    FlatStoreError::Corrupt("source mutation heap underflow".to_owned())
                })?;
                let generation = self.cursors[index].descriptor.generation;
                let mutation = self.cursors[index].current;
                if authority.is_none_or(|(prior, _)| generation > prior) {
                    authority = Some((generation, mutation));
                }
                if self.cursors[index].advance()? {
                    self.heap
                        .push(Reverse((self.cursors[index].current.event_id, index)));
                }
            }
            let (_, mutation) = authority.ok_or_else(|| {
                FlatStoreError::Corrupt("source mutation merge lost authority".to_owned())
            })?;
            if mutation.kind == MutationKind::Delete {
                continue;
            }
            let vector_count = self
                .vectors
                .get(&mutation.vector_generation)
                .ok_or_else(|| {
                    FlatStoreError::Corrupt(format!(
                        "event {} references absent source vector generation",
                        mutation.event_id
                    ))
                })?;
            let end = mutation
                .first_vector_ordinal
                .checked_add(u64::from(mutation.chunk_count))
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("source vector range overflow".to_owned())
                })?;
            if end > *vector_count {
                return Err(FlatStoreError::Corrupt(format!(
                    "event {} has an out-of-range source vector locator",
                    mutation.event_id
                )));
            }
            self.active_events = self.active_events.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt("source active event overflow".to_owned())
            })?;
            self.active_chunks = self
                .active_chunks
                .checked_add(u64::from(mutation.chunk_count))
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("source active chunk overflow".to_owned())
                })?;
            return Ok(Some(FlatActiveEvent {
                event_id: mutation.event_id,
                seq: mutation.seq,
                source_text_hash: mutation.source_text_hash,
                chunk_count: mutation.chunk_count,
                source_identity_digest: self.source.source_identity_digest.clone(),
                source_reconciliation_id: self.source.source_reconciliation_id.clone(),
                stable_identity_hash: mutation.stable_identity_hash,
                vector_generation: mutation.vector_generation,
                first_vector_ordinal: mutation.first_vector_ordinal,
            }));
        }
    }
}

impl SourceMutationCursor {
    fn new(
        root: &Path,
        contract: &FlatModelContract,
        descriptor: &SegmentDescriptor,
    ) -> FlatResult<Self> {
        let mapping = map_artifact(
            root,
            descriptor,
            &descriptor.mutations,
            ArtifactRole::Mutations,
            contract,
        )?;
        let header = decode_header(&mapping)?;
        let record_count = usize_from_u64(header.record_count, "source mutation count")?;
        let current = mutation_at(&mapping, 0)?;
        validate_mutation_for_segment(&current, descriptor.kind, descriptor.generation)?;
        Ok(Self {
            descriptor: descriptor.clone(),
            mapping,
            next_ordinal: 1,
            record_count,
            previous_event_id: Some(current.event_id),
            current,
        })
    }

    fn advance(&mut self) -> FlatResult<bool> {
        if self.next_ordinal >= self.record_count {
            return Ok(false);
        }
        let mutation = mutation_at(&self.mapping, self.next_ordinal)?;
        if self
            .previous_event_id
            .is_some_and(|previous| previous >= mutation.event_id)
        {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {} mutations are not uniquely sorted",
                self.descriptor.generation
            )));
        }
        validate_mutation_for_segment(&mutation, self.descriptor.kind, self.descriptor.generation)?;
        self.previous_event_id = Some(mutation.event_id);
        self.current = mutation;
        self.next_ordinal += 1;
        Ok(true)
    }
}

fn mutation_at(mapping: &[u8], ordinal: usize) -> FlatResult<EventMutation> {
    let start =
        HEADER_BYTES
            .checked_add(ordinal.checked_mul(MUTATION_RECORD_BYTES).ok_or_else(|| {
                FlatStoreError::Corrupt("source mutation offset overflow".to_owned())
            })?)
            .ok_or_else(|| FlatStoreError::Corrupt("source mutation offset overflow".to_owned()))?;
    let record = mapping
        .get(start..start + MUTATION_RECORD_BYTES)
        .ok_or_else(|| FlatStoreError::Corrupt("source mutation record is truncated".to_owned()))?;
    decode_mutation_record(record)
}

fn publication_order(
    expected: &FlatPublicationToken,
    current: &FlatPublicationToken,
) -> FlatResult<std::cmp::Ordering> {
    match expected.generation.cmp(&current.generation) {
        std::cmp::Ordering::Equal if expected.generation_hash != current.generation_hash => {
            Err(FlatStoreError::Corrupt(
                "Flat publication generation has a different manifest hash".to_owned(),
            ))
        }
        ordering => Ok(ordering),
    }
}

fn validate_retained_candidate(
    finalized: &SourceStageFinal,
    source: &FlatSourceScope,
    baseline: &FlatPublicationToken,
    active: &FlatPublicationToken,
    manifest: Option<&SelectedManifest>,
) -> FlatResult<()> {
    if finalized.schema_version != SOURCE_STAGE_SCHEMA_VERSION
        || &finalized.source != source
        || &finalized.baseline_publication != baseline
        || &finalized.candidate_publication != active
    {
        return Err(FlatStoreError::Corrupt(
            "retained Flat source candidate disagrees with active state".to_owned(),
        ));
    }
    let manifest = manifest.ok_or_else(|| {
        FlatStoreError::Corrupt("retained Flat candidate manifest is missing".to_owned())
    })?;
    let receipt = manifest
        .envelope
        .manifest
        .source_snapshots
        .iter()
        .find(|snapshot| snapshot.source_identity_digest == source.source_identity_digest)
        .and_then(|snapshot| snapshot.receipt.as_ref());
    if receipt != finalized.receipt.as_ref() {
        return Err(FlatStoreError::Corrupt(
            "retained Flat candidate receipt disagrees with its staging record".to_owned(),
        ));
    }
    if !manifest
        .envelope
        .manifest
        .segments
        .iter()
        .any(|descriptor| descriptor == &finalized.catalog)
    {
        return Err(FlatStoreError::Corrupt(
            "retained Flat candidate catalog disagrees with its staging record".to_owned(),
        ));
    }
    Ok(())
}
