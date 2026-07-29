use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
};

use anyhow::Result;
use ctx_history_core::utc_now;
use ctx_history_store::Store;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use super::{
    document::semantic_event_document_from_store_projection,
    indexing::{semantic_document_hash, semantic_source_text},
    model_contract::SEMANTIC_DIMENSIONS,
    runtime_limits::SEMANTIC_PRUNE_EVENTS_PER_PASS,
    vector_store::{
        flat_segments::{FlatChunk, FlatEventReplacement, FlatSourceHash, PinnedFlatGeneration},
        SemanticChunkDocument, SemanticPruneOutcome, SemanticSidecarStats, SemanticStoredEvent,
        SemanticVectorStore,
    },
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
    SemanticEventDocument,
};

const BACKFILL_CURSOR: &str = "backfill_cursor_before";
const COMMITTED_RECONCILIATION_CURSOR: &str = "committed_store_reconcile_cursor_before";
const PRUNE_CURSOR: &str = "prune_anchor_cursor_before";
const COMPACT_SEGMENT_THRESHOLD: usize = 16;

impl SemanticVectorStore {
    fn required_dirty_state(connection: &Connection) -> Result<usize> {
        let dirty = connection
            .query_row(
                "SELECT dirty_items FROM semantic_index_stats WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic control metadata is missing its cached dirty count",
                )
            })?;
        usize::try_from(dirty).map_err(|_| {
            SemanticVectorStoreError::reset_required(
                "semantic control metadata has a negative dirty count",
            )
            .into()
        })
    }

    pub(super) fn cached_stats(&self) -> Result<Option<SemanticSidecarStats>> {
        semantic_owned_sidecar_result(self.flat_stats().map(Some))
    }

    pub(super) fn cached_or_exact_stats(&self) -> Result<SemanticSidecarStats> {
        semantic_owned_sidecar_result(self.flat_stats())
    }

    pub(super) fn exact_stats(&self) -> Result<SemanticSidecarStats> {
        semantic_owned_sidecar_result(self.flat_stats())
    }

    fn apply_dirty_delta(
        transaction: &rusqlite::Transaction<'_>,
        removed: usize,
        inserted: usize,
    ) -> Result<()> {
        let current = Self::required_dirty_state(transaction)?;
        let dirty = current
            .checked_sub(removed)
            .and_then(|value| value.checked_add(inserted))
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic dirty cached-count delta overflowed",
                )
            })?;
        let changed = transaction.execute(
            "UPDATE semantic_index_stats SET dirty_items = ?1 WHERE id = 1",
            [i64::try_from(dirty)?],
        )?;
        if changed != 1 {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic control metadata lost its dirty-count row",
            )
            .into());
        }
        Ok(())
    }

    fn maintenance_cursor<A, B>(&self, key: &str) -> Result<Option<(A, B)>>
    where
        A: std::str::FromStr,
        B: std::str::FromStr,
    {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some((first, second)) = value.as_deref().and_then(|value| value.split_once(':')) else {
            return Ok(None);
        };
        Ok(first.parse().ok().zip(second.parse().ok()))
    }

    fn set_maintenance_cursor<A: fmt::Display, B: fmt::Display>(
        &self,
        key: &str,
        cursor: Option<(A, B)>,
    ) -> Result<()> {
        match cursor {
            Some((first, second)) => {
                self.conn.execute(
                    "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, format!("{first}:{second}")],
                )?;
            }
            None => {
                self.conn.execute(
                    "DELETE FROM semantic_maintenance_state WHERE key = ?1",
                    [key],
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn backfill_cursor(&self) -> Result<Option<(i64, u64)>> {
        semantic_owned_sidecar_result(self.maintenance_cursor(BACKFILL_CURSOR))
    }

    pub(super) fn set_backfill_cursor(&self, cursor: Option<(i64, u64)>) -> Result<()> {
        semantic_owned_sidecar_result(self.set_maintenance_cursor(BACKFILL_CURSOR, cursor))
    }

    pub(super) fn committed_store_reconciliation_cursor(&self) -> Result<Option<(i64, u64)>> {
        semantic_owned_sidecar_result(self.maintenance_cursor(COMMITTED_RECONCILIATION_CURSOR))
    }

    pub(super) fn set_committed_store_reconciliation_cursor(
        &self,
        cursor: Option<(i64, u64)>,
    ) -> Result<()> {
        semantic_owned_sidecar_result(
            self.set_maintenance_cursor(COMMITTED_RECONCILIATION_CURSOR, cursor),
        )
    }

    pub(super) fn dirty_event_count(&self) -> Result<usize> {
        semantic_owned_sidecar_result(Self::required_dirty_state(&self.conn))
    }

    pub(super) fn enqueue_dirty_documents(
        &mut self,
        documents: &[SemanticEventDocument],
        reason: &str,
    ) -> Result<usize> {
        semantic_owned_sidecar_result((|| {
            let transaction = self.conn.transaction()?;
            let changed = Self::enqueue_dirty_in_transaction(&transaction, documents, reason)?;
            transaction.commit()?;
            Ok(changed)
        })())
    }

    fn enqueue_dirty_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        documents: &[SemanticEventDocument],
        reason: &str,
    ) -> Result<usize> {
        if documents.is_empty() {
            return Ok(0);
        }
        let ids = documents
            .iter()
            .map(|document| document.event_id)
            .collect::<HashSet<_>>();
        let mut existing = 0;
        {
            let mut statement = transaction.prepare(
                "SELECT EXISTS(SELECT 1 FROM semantic_dirty_events WHERE event_id = ?1)",
            )?;
            for event_id in &ids {
                existing += statement
                    .query_row([event_id.to_string()], |row| row.get::<_, bool>(0))?
                    as usize;
            }
        }
        let reason = reason.chars().take(64).collect::<String>();
        let queued_at_ms = utc_now().timestamp_millis();
        let mut changed = 0;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO semantic_dirty_events
                 (event_id, queued_at_ms, priority_seq, reason, attempts)
                 VALUES (?1, ?2, ?3, ?4, 0)
                 ON CONFLICT(event_id) DO UPDATE SET
                    queued_at_ms = excluded.queued_at_ms,
                    priority_seq = COALESCE(excluded.priority_seq, semantic_dirty_events.priority_seq),
                    reason = excluded.reason",
            )?;
            for document in documents {
                changed += statement.execute(params![
                    document.event_id.to_string(),
                    queued_at_ms,
                    i64::try_from(document.seq)?,
                    reason
                ])?;
            }
        }
        Self::apply_dirty_delta(transaction, 0, ids.len().saturating_sub(existing))?;
        Ok(changed)
    }

    pub(super) fn queued_dirty_event_ids(&self, limit: usize) -> Result<Vec<Uuid>> {
        semantic_owned_sidecar_result((|| {
            if limit == 0 {
                return Ok(Vec::new());
            }
            let mut statement = self.conn.prepare(
                "SELECT event_id FROM semantic_dirty_events
                 ORDER BY priority_seq DESC, queued_at_ms ASC, event_id ASC LIMIT ?1",
            )?;
            let rows =
                statement.query_map([i64::try_from(limit)?], |row| row.get::<_, String>(0))?;
            rows.map(|row| {
                let value = row?;
                Uuid::parse_str(&value).map_err(|_| {
                    SemanticVectorStoreError::reset_required(format!(
                        "invalid dirty event id in semantic control metadata: {value}"
                    ))
                    .into()
                })
            })
            .collect()
        })())
    }

    pub(super) fn dequeue_dirty_events(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        semantic_owned_sidecar_result((|| {
            let ids = event_ids.iter().copied().collect::<HashSet<_>>();
            let transaction = self.conn.transaction()?;
            let mut deleted = 0;
            {
                let mut statement =
                    transaction.prepare("DELETE FROM semantic_dirty_events WHERE event_id = ?1")?;
                for event_id in ids {
                    deleted += statement.execute([event_id.to_string()])?;
                }
            }
            Self::apply_dirty_delta(&transaction, deleted, 0)?;
            transaction.commit()?;
            Ok(deleted)
        })())
    }

    pub(super) fn plaintext_value_count(&self) -> Result<usize> {
        Ok(0)
    }

    pub(super) fn existing_hashes_for_event_ids(
        &self,
        event_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, String>> {
        semantic_owned_sidecar_result(self.flat_existing_hashes(event_ids))
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

    pub(super) fn prune_ineligible_events(
        &mut self,
        store: &Store,
    ) -> Result<SemanticPruneOutcome> {
        let cursor = self.maintenance_cursor::<u64, Uuid>(PRUNE_CURSOR)?;
        let events = self.prune_candidates(cursor)?;
        if events.is_empty() {
            if cursor.is_some() {
                self.set_maintenance_cursor(PRUNE_CURSOR, None::<(u64, Uuid)>)?;
            }
            return Ok(SemanticPruneOutcome {
                scan_complete: true,
                ..SemanticPruneOutcome::default()
            });
        }
        let mut outcome = SemanticPruneOutcome {
            scanned_events: events.len(),
            scan_complete: events.len() < SEMANTIC_PRUNE_EVENTS_PER_PASS,
            ..SemanticPruneOutcome::default()
        };
        let ids = events
            .iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>();
        let eligible = store.semantic_eligible_event_ids(&ids)?;
        let current = store
            .event_embedding_documents_by_ids(&ids)?
            .into_iter()
            .map(|document| {
                let document = semantic_event_document_from_store_projection!(document);
                (document.event_id, document)
            })
            .collect::<HashMap<_, _>>();
        let mut delete = Vec::new();
        let mut stale = Vec::new();
        for event in &events {
            let Some(document) = current.get(&event.event_id) else {
                delete.push(event.event_id);
                continue;
            };
            if !eligible.contains(&event.event_id) {
                delete.push(event.event_id);
                continue;
            }
            let text = semantic_source_text(&document.text);
            if semantic_document_hash(document, &text) != event.source_text_hash {
                delete.push(event.event_id);
                stale.push(document.clone());
            }
        }
        outcome.deleted_chunks = self.delete_events(&delete)?;
        if !stale.is_empty() {
            outcome.queued_stale_events = self.enqueue_dirty_documents(&stale, "stale_hash")?;
        }
        if outcome.scan_complete {
            self.set_maintenance_cursor(PRUNE_CURSOR, None::<(u64, Uuid)>)?;
        } else if let Some(event) = events.last() {
            self.set_maintenance_cursor(PRUNE_CURSOR, Some((event.seq, event.event_id)))?;
        }
        Ok(outcome)
    }

    fn prune_candidates(&self, cursor: Option<(u64, Uuid)>) -> Result<Vec<SemanticStoredEvent>> {
        let mut events = self.flat_active_events()?;
        events.sort_by(|left, right| {
            right
                .seq
                .cmp(&left.seq)
                .then_with(|| right.event_id.cmp(&left.event_id))
        });
        if let Some(cursor) = cursor {
            events.retain(|event| (event.seq, event.event_id) < cursor);
        }
        events.truncate(SEMANTIC_PRUNE_EVENTS_PER_PASS);
        Ok(events)
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

    #[cfg(test)]
    pub(super) fn delete_embedding_chunks_for_event_ids(
        &mut self,
        event_ids: &[Uuid],
    ) -> Result<usize> {
        self.delete_events(event_ids)
    }

    pub(super) fn flat_pin_generation(&self) -> Result<Option<PinnedFlatGeneration>> {
        self.flat.pin_generation().map_err(anyhow::Error::new)
    }

    fn flat_stats(&self) -> Result<SemanticSidecarStats> {
        let stats = self.flat.active_stats().map_err(anyhow::Error::new)?;
        Ok(SemanticSidecarStats {
            embedded_items: stats.active_events,
            embedded_chunks: stats.active_chunks,
        })
    }

    fn flat_existing_hashes(&self, event_ids: &[Uuid]) -> Result<HashMap<Uuid, String>> {
        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let requested = event_ids.iter().copied().collect::<HashSet<_>>();
        Ok(self
            .flat
            .active_events()
            .map_err(anyhow::Error::new)?
            .into_iter()
            .filter(|event| requested.contains(&event.event_id))
            .map(|event| (event.event_id, event.source_text_hash.to_hex()))
            .collect())
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
