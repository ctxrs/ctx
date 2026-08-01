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
        SemanticChunkDocument, SemanticStoredEvent, SemanticVectorStore,
    },
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
};

const COMPACT_SEGMENT_THRESHOLD: usize = 16;

impl SemanticVectorStore {
    pub(super) fn flat_active_event_lookup(&self) -> Result<FlatActiveEventLookup> {
        semantic_owned_sidecar_result(self.flat.active_event_lookup().map_err(anyhow::Error::new))
    }

    pub(super) fn upsert_chunk_embeddings(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
    ) -> Result<()> {
        semantic_owned_sidecar_result((|| {
            if items.is_empty() {
                return Ok(());
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
            self.flat_publish_upsert(items)
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
            let deleted = self.flat_publish_delete(event_ids)?;
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

    fn flat_publish_upsert(&mut self, items: &[(SemanticChunkDocument, Vec<f32>)]) -> Result<()> {
        let mut grouped = BTreeMap::<Uuid, (u64, FlatSourceHash, Vec<FlatChunk>)>::new();
        for (document, embedding) in items {
            let source_hash = FlatSourceHash::parse_hex(&document.source_text_hash)
                .map_err(anyhow::Error::new)?;
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
        let replacements = grouped
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
            .collect::<Vec<_>>();
        self.flat
            .publish_replacement_event_chunks(&replacements, &[])
            .map_err(anyhow::Error::new)?;
        self.flat_compact_if_needed()?;
        Ok(())
    }

    fn flat_publish_delete(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        let requested = event_ids.iter().copied().collect::<HashSet<_>>();
        let deleted = self
            .flat
            .active_events()
            .map_err(anyhow::Error::new)?
            .into_iter()
            .filter(|event| requested.contains(&event.event_id))
            .try_fold(0_usize, |count, event| {
                count
                    .checked_add(event.chunk_count as usize)
                    .ok_or_else(|| {
                        anyhow::Error::new(SemanticVectorStoreError::reset_required(
                            "semantic deleted chunk count overflowed",
                        ))
                    })
            })?;
        self.flat
            .delete_events(event_ids)
            .map_err(anyhow::Error::new)?;
        self.flat_compact_if_needed()?;
        Ok(deleted)
    }

    pub(super) fn flat_active_events(&self) -> Result<Vec<SemanticStoredEvent>> {
        self.flat
            .active_events()
            .map_err(anyhow::Error::new)?
            .into_iter()
            .map(|event| {
                Ok(SemanticStoredEvent {
                    event_id: event.event_id,
                    source_text_hash: event.source_text_hash.to_hex(),
                    seq: event.seq,
                })
            })
            .collect()
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
