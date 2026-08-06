use super::*;

pub(super) const UNSCOPED_SOURCE_IDENTITY: &str = "flat-unscoped-source";
pub(super) const UNSCOPED_RECONCILIATION_ID: &str = "flat-unscoped-reconciliation";

pub(super) fn unscoped_source() -> FlatSourceScope {
    FlatSourceScope {
        source_identity_digest: UNSCOPED_SOURCE_IDENTITY.to_owned(),
        source_reconciliation_id: UNSCOPED_RECONCILIATION_ID.to_owned(),
    }
}

pub(super) fn source_snapshot_generation(manifest: &Manifest, source: &str) -> u64 {
    manifest
        .source_snapshots
        .binary_search_by(|snapshot| snapshot.source_identity_digest.as_str().cmp(source))
        .ok()
        .map_or(0, |index| manifest.source_snapshots[index].generation)
}

pub(super) fn set_source_snapshot(
    manifest: &mut Manifest,
    source_identity_digest: &str,
    generation: u64,
) {
    match manifest.source_snapshots.binary_search_by(|snapshot| {
        snapshot
            .source_identity_digest
            .as_str()
            .cmp(source_identity_digest)
    }) {
        Ok(index) => manifest.source_snapshots[index].generation = generation,
        Err(index) => manifest.source_snapshots.insert(
            index,
            SourceSnapshot {
                source_identity_digest: source_identity_digest.to_owned(),
                generation,
                receipt: None,
            },
        ),
    }
}

pub(super) fn set_source_snapshot_receipt(
    manifest: &mut Manifest,
    source_identity_digest: &str,
    generation: u64,
    receipt: FlatSourceReceipt,
) {
    match manifest.source_snapshots.binary_search_by(|snapshot| {
        snapshot
            .source_identity_digest
            .as_str()
            .cmp(source_identity_digest)
    }) {
        Ok(index) => {
            manifest.source_snapshots[index].generation = generation;
            manifest.source_snapshots[index].receipt = Some(receipt);
        }
        Err(index) => manifest.source_snapshots.insert(
            index,
            SourceSnapshot {
                source_identity_digest: source_identity_digest.to_owned(),
                generation,
                receipt: Some(receipt),
            },
        ),
    }
}

pub(super) fn remove_source_snapshot(manifest: &mut Manifest, source_identity_digest: &str) {
    if let Ok(index) = manifest.source_snapshots.binary_search_by(|snapshot| {
        snapshot
            .source_identity_digest
            .as_str()
            .cmp(source_identity_digest)
    }) {
        manifest.source_snapshots.remove(index);
    }
}

pub(super) fn manifest_source_states(manifest: &Manifest) -> Vec<FlatSourceState> {
    let mut states = manifest
        .source_snapshots
        .iter()
        .map(|snapshot| {
            (
                snapshot.source_identity_digest.clone(),
                (snapshot.generation, snapshot.receipt.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for segment in &manifest.segments {
        if segment.source_identity_digest == UNSCOPED_SOURCE_IDENTITY {
            continue;
        }
        match states.entry(segment.source_identity_digest.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((0, None));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if segment.generation > entry.get().0 {
                    entry.get_mut().1 = None;
                }
            }
        }
    }
    states
        .into_iter()
        .map(|(source_identity_digest, (_, receipt))| FlatSourceState {
            source_identity_digest,
            receipt,
        })
        .collect()
}

pub(super) fn load_active_events(
    root: &Path,
    contract: &FlatModelContract,
    manifest: &Manifest,
    source_filter: Option<&str>,
) -> FlatResult<(Arc<Vec<FlatActiveEvent>>, u64)> {
    let mut active = BTreeMap::<Uuid, FlatActiveEvent>::new();
    let mut touched = 0_u64;
    for descriptor in &manifest.segments {
        if source_filter.is_some_and(|source| descriptor.source_identity_digest != source) {
            continue;
        }
        let floor = source_snapshot_generation(manifest, &descriptor.source_identity_digest);
        if descriptor.generation < floor {
            continue;
        }
        let mutations = load_catalog_mutations(root, contract, descriptor)?;
        touched = touched
            .checked_add(u64::try_from(mutations.len()).map_err(|_| {
                FlatStoreError::Corrupt("catalog mutation count does not fit u64".to_owned())
            })?)
            .ok_or_else(|| FlatStoreError::Corrupt("catalog touch count overflow".to_owned()))?;
        for mutation in mutations {
            match mutation.kind {
                MutationKind::Delete => {
                    active.remove(&mutation.event_id);
                }
                MutationKind::Replace => {
                    validate_event_locator(manifest, descriptor, &mutation)?;
                    if active.get(&mutation.event_id).is_some_and(|prior| {
                        prior.source_identity_digest != descriptor.source_identity_digest
                    }) {
                        return Err(FlatStoreError::Corrupt(format!(
                            "event {} is owned by more than one flat source",
                            mutation.event_id
                        )));
                    }
                    active.insert(
                        mutation.event_id,
                        FlatActiveEvent {
                            event_id: mutation.event_id,
                            seq: mutation.seq,
                            source_text_hash: mutation.source_text_hash,
                            chunk_count: mutation.chunk_count,
                            source_identity_digest: descriptor.source_identity_digest.clone(),
                            source_reconciliation_id: descriptor.source_reconciliation_id.clone(),
                            stable_identity_hash: mutation.stable_identity_hash,
                            vector_generation: mutation.vector_generation,
                            first_vector_ordinal: mutation.first_vector_ordinal,
                        },
                    );
                }
            }
        }
    }
    Ok((Arc::new(active.into_values().collect()), touched))
}

fn validate_event_locator(
    manifest: &Manifest,
    authority: &SegmentDescriptor,
    mutation: &EventMutation,
) -> FlatResult<()> {
    let vector_segment = manifest
        .segments
        .binary_search_by_key(&mutation.vector_generation, |segment| segment.generation)
        .ok()
        .map(|index| &manifest.segments[index])
        .ok_or_else(|| {
            FlatStoreError::Corrupt(format!(
                "event {} references absent vector generation {}",
                mutation.event_id, mutation.vector_generation
            ))
        })?;
    let end = mutation
        .first_vector_ordinal
        .checked_add(u64::from(mutation.chunk_count))
        .ok_or_else(|| FlatStoreError::Corrupt("event vector range overflow".to_owned()))?;
    if vector_segment.source_identity_digest != authority.source_identity_digest
        || end > vector_segment.vector_count
    {
        return Err(FlatStoreError::Corrupt(format!(
            "event {} has a cross-source or out-of-range vector locator",
            mutation.event_id
        )));
    }
    Ok(())
}

pub(super) fn event_mutation(event: &FlatActiveEvent) -> EventMutation {
    EventMutation {
        event_id: event.event_id,
        kind: MutationKind::Replace,
        seq: event.seq,
        source_text_hash: event.source_text_hash,
        stable_identity_hash: event.stable_identity_hash,
        vector_generation: event.vector_generation,
        first_vector_ordinal: event.first_vector_ordinal,
        chunk_count: event.chunk_count,
    }
}

pub(super) fn manifest_stats(selected: &SelectedManifest) -> FlatActiveStats {
    let manifest = &selected.envelope.manifest;
    let stored_chunks = manifest.segments.iter().fold(0_u64, |total, segment| {
        total.saturating_add(segment.vector_count)
    });
    let dimensions = u64::from(manifest.model.dimensions);
    FlatActiveStats {
        generation: manifest.generation,
        generation_hash: Some(selected.generation_hash.clone()),
        segment_count: manifest.segments.len(),
        active_events: usize::try_from(manifest.active_events).unwrap_or(usize::MAX),
        active_chunks: usize::try_from(manifest.active_chunks).unwrap_or(usize::MAX),
        active_vector_bytes: manifest
            .active_chunks
            .saturating_mul(dimensions)
            .saturating_mul(4),
        stored_chunks,
        stored_vector_bytes: stored_chunks.saturating_mul(dimensions).saturating_mul(4),
        deleted_events: 0,
    }
}

pub(super) fn apply_publication_counts(
    manifest: &mut Manifest,
    existing: &FlatActiveEventLookup,
    replacements: &[FlatEventReplacement],
    tombstones: &[Uuid],
) -> FlatResult<()> {
    for replacement in replacements {
        let old_chunks = existing
            .event(replacement.event_id)
            .map_or(0_u64, |event| u64::from(event.chunk_count));
        let new_chunks = u64::try_from(replacement.chunks.len()).map_err(|_| {
            FlatStoreError::InvalidInput("replacement chunk count is too large".to_owned())
        })?;
        if old_chunks == 0 {
            manifest.active_events = manifest.active_events.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt("manifest active event count overflow".to_owned())
            })?;
        }
        manifest.active_chunks = manifest
            .active_chunks
            .checked_sub(old_chunks)
            .and_then(|value| value.checked_add(new_chunks))
            .ok_or_else(|| {
                FlatStoreError::Corrupt("manifest active chunk count overflow".to_owned())
            })?;
    }
    for event_id in tombstones {
        let Some(event) = existing.event(*event_id) else {
            continue;
        };
        manifest.active_events = manifest.active_events.checked_sub(1).ok_or_else(|| {
            FlatStoreError::Corrupt("manifest active event count underflow".to_owned())
        })?;
        manifest.active_chunks = manifest
            .active_chunks
            .checked_sub(u64::from(event.chunk_count))
            .ok_or_else(|| {
                FlatStoreError::Corrupt("manifest active chunk count underflow".to_owned())
            })?;
    }
    Ok(())
}
