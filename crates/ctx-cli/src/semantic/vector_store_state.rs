use std::collections::{BTreeMap, HashSet};

use anyhow::Result;
use uuid::Uuid;

use super::{
    model_contract::SEMANTIC_DIMENSIONS,
    vector_store::{
        flat_segments::{
            FlatActiveEventLookup, FlatChunk, FlatEventReplacement, FlatSourceHash,
            PinnedFlatGeneration,
        },
        SemanticChunkDocument, SemanticVectorStore,
    },
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
};

const COMPACT_SEGMENT_THRESHOLD: usize = 16;

impl SemanticVectorStore {
    pub(super) fn flat_active_event_lookup(&self) -> Result<FlatActiveEventLookup> {
        semantic_owned_sidecar_result(self.flat.active_event_lookup().map_err(anyhow::Error::new))
    }

    /// Publishes all replacements and retirements for one bounded source page
    /// in a single flat-store generation.
    pub(super) fn publish_chunk_replacements(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
        event_ids: &[Uuid],
    ) -> Result<usize> {
        semantic_owned_sidecar_result((|| {
            if items.is_empty() && event_ids.is_empty() {
                return Ok(0);
            }
            if items.iter().any(|(_, embedding)| {
                embedding.len() != SEMANTIC_DIMENSIONS
                    || embedding.iter().any(|value| !value.is_finite())
            }) {
                return Err(SemanticVectorStoreError::unavailable(format!(
                    "semantic embeddings must be finite f32[{SEMANTIC_DIMENSIONS}]"
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
            self.flat
                .publish_replacement_event_chunks(
                    &replacements,
                    &tombstones.into_iter().collect::<Vec<_>>(),
                )
                .map_err(anyhow::Error::new)?;
            self.flat_compact_if_needed()?;
            Ok(deleted)
        })())
    }

    pub(super) fn delete_events(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        semantic_owned_sidecar_result((|| {
            if event_ids.is_empty() {
                return Ok(0);
            }
            // Flat publication happens first. A crash before the following
            // metadata cleanup leaves a safe, repeatable tombstone rather than
            // exposing stale vectors.
            let deleted = self.publish_chunk_replacements(&[], event_ids)?;
            let transaction = self.conn.transaction()?;
            {
                let mut delete_source_metadata = transaction
                    .prepare("DELETE FROM semantic_source_documents WHERE event_id = ?1")?;
                for event_id in event_ids.iter().copied().collect::<HashSet<_>>() {
                    delete_source_metadata.execute([event_id.to_string()])?;
                }
            }
            transaction.commit()?;
            Ok(deleted)
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
        let stats = self.flat.active_stats().map_err(anyhow::Error::new)?;
        if stats.segment_count >= COMPACT_SEGMENT_THRESHOLD
            || (stats.active_chunks > 0
                && stats.stored_chunks > (stats.active_chunks as u64).saturating_mul(2))
        {
            self.flat.compact().map_err(anyhow::Error::new)?;
        }
        Ok(())
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
