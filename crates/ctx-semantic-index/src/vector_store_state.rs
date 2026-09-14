use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use uuid::Uuid;

use super::{
    vector_store::{
        flat_segments::{
            FlatActiveEventLookup, FlatChunk, FlatEventMetadataUpdate, FlatEventReplacement,
            FlatSourceHash, FlatSourcePageOutcome, PinnedFlatGeneration,
        },
        SemanticChunkDocument, SemanticVectorStore,
    },
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
};

impl SemanticVectorStore {
    pub(super) fn flat_active_event_lookup(&self) -> Result<FlatActiveEventLookup> {
        semantic_owned_sidecar_result(self.flat.active_event_lookup().map_err(anyhow::Error::new))
    }

    /// Publishes all replacements and retirements for one bounded source page
    /// in a single flat-store generation.
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn publish_chunk_replacements(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
        event_ids: &[Uuid],
    ) -> Result<usize> {
        self.publish_chunk_replacements_with_coordination(items, event_ids, false)
    }

    fn publish_chunk_replacements_coordinated(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
        event_ids: &[Uuid],
    ) -> Result<usize> {
        self.publish_chunk_replacements_with_coordination(items, event_ids, true)
    }

    fn publish_chunk_replacements_with_coordination(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
        event_ids: &[Uuid],
        transaction_held: bool,
    ) -> Result<usize> {
        let dimensions = self.contract().dimensions();
        semantic_owned_sidecar_result((|| {
            if items.is_empty() && event_ids.is_empty() {
                return Ok(0);
            }
            if items.iter().any(|(_, embedding)| {
                embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
            }) {
                return Err(SemanticVectorStoreError::unavailable(format!(
                    "semantic embeddings must be finite f32[{dimensions}]"
                ))
                .into());
            }
            let tombstones = event_ids.iter().copied().collect::<HashSet<_>>();
            if items
                .iter()
                .any(|(document, _)| tombstones.contains(&document.event_id))
            {
                return Err(SemanticVectorStoreError::storage_conflict(
                    "semantic page cannot replace and retire the same event",
                )
                .into());
            }
            let replacements = grouped_replacements(items)?;
            let lookup = self.flat_active_event_lookup()?;
            let deleted = tombstones.iter().try_fold(0_usize, |count, event_id| {
                let chunks = lookup
                    .event(*event_id)
                    .map_or(0_usize, |event| event.chunk_count as usize);
                count.checked_add(chunks).ok_or_else(|| {
                    anyhow::Error::new(SemanticVectorStoreError::reset_required(
                        "semantic deleted chunk count overflowed",
                    ))
                })
            })?;
            let tombstones = tombstones.into_iter().collect::<Vec<_>>();
            if transaction_held {
                self.flat
                    .publish_replacement_event_chunks_coordinated(&replacements, &tombstones)
            } else {
                self.flat
                    .publish_replacement_event_chunks(&replacements, &tombstones)
            }
            .map_err(anyhow::Error::new)?;
            self.flat_compact_if_needed()?;
            Ok(deleted)
        })())
    }

    pub(super) fn publish_source_page(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
        authority_updates: &[FlatEventMetadataUpdate],
        event_ids: &[Uuid],
        existing: &FlatActiveEventLookup,
    ) -> Result<FlatSourcePageOutcome> {
        let dimensions = self.contract().dimensions();
        semantic_owned_sidecar_result((|| {
            if items.iter().any(|(_, embedding)| {
                embedding.len() != dimensions || embedding.iter().any(|value| !value.is_finite())
            }) {
                return Err(SemanticVectorStoreError::unavailable(format!(
                    "semantic embeddings must be finite f32[{dimensions}]"
                ))
                .into());
            }
            let replacements = grouped_replacements(items)?;
            let publication = self
                .flat
                .publish_source_event_page(&replacements, authority_updates, event_ids, existing)
                .map_err(anyhow::Error::new)?;
            Ok(publication)
        })())
    }

    pub(super) fn delete_events_coordinated(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        semantic_owned_sidecar_result((|| {
            if event_ids.is_empty() {
                return Ok(0);
            }
            self.publish_chunk_replacements_coordinated(&[], event_ids)
        })())
    }

    #[cfg(test)]
    pub(super) fn delete_events(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        semantic_owned_sidecar_result((|| {
            if event_ids.is_empty() {
                return Ok(0);
            }
            self.publish_chunk_replacements(&[], event_ids)
        })())
    }

    pub(super) fn flat_pin_generation(&self) -> Result<Option<PinnedFlatGeneration>> {
        self.flat.pin_generation().map_err(anyhow::Error::new)
    }

    #[cfg(test)]
    pub(super) fn reset_flat_active_event_snapshot_count(&self) {
        self.flat.reset_active_event_snapshot_count();
    }

    #[cfg(test)]
    pub(super) fn flat_active_event_snapshot_count(&self) -> u64 {
        self.flat.active_event_snapshot_count()
    }

    fn flat_compact_if_needed(&mut self) -> Result<()> {
        if self
            .flat
            .reconciliation_active()
            .map_err(anyhow::Error::new)?
        {
            // One immutable delta is published per bounded reconciliation
            // page. The retained view performs exact stats and threshold
            // compaction once when that persisted reconciliation terminates.
            return Ok(());
        }
        self.flat.compact_if_needed().map_err(anyhow::Error::new)
    }
}

fn grouped_replacements(
    items: &[(SemanticChunkDocument, Vec<f32>)],
) -> Result<Vec<FlatEventReplacement>> {
    let mut grouped = BTreeMap::<Uuid, (u64, FlatSourceHash, Vec<FlatChunk>)>::new();
    for (document, embedding) in items {
        let source_hash =
            FlatSourceHash::parse_hex(&document.source_text_hash).map_err(anyhow::Error::new)?;
        let chunk = FlatChunk {
            chunk_index: u32::try_from(document.chunk_index)?,
            start_char: u32::try_from(document.start_char)?,
            end_char: u32::try_from(document.end_char)?,
            vector: embedding.clone(),
        };
        match grouped.entry(document.event_id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((document.seq, source_hash, vec![chunk]));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let (seq, existing_hash, chunks) = entry.get_mut();
                if *seq != document.seq || *existing_hash != source_hash {
                    return Err(SemanticVectorStoreError::storage_conflict(format!(
                        "semantic chunks for {} disagree on sequence or source hash",
                        document.event_id
                    ))
                    .into());
                }
                chunks.push(chunk);
            }
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(event_id, (seq, source_text_hash, mut chunks))| {
            chunks.sort_by_key(|chunk| chunk.chunk_index);
            FlatEventReplacement {
                event_id,
                seq,
                source_text_hash,
                chunks,
            }
        })
        .collect())
}
