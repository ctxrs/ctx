use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use anyhow::Result;
use ctx_history_core::utc_now;
use ctx_history_store::{EventEmbeddingDocument, Store};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension};
use uuid::Uuid;

use super::{
    indexing::{semantic_document_hash, semantic_source_text, serialize_f32_blob},
    model_contract::SEMANTIC_DIMENSIONS,
    runtime_limits::{SEMANTIC_PRUNE_EVENTS_PER_PASS, SEMANTIC_PRUNE_EVENT_BATCH},
    vector_store::{
        SemanticChunkDocument, SemanticPruneOutcome, SemanticSidecarStats, SemanticVectorStore,
    },
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
};

const BACKFILL_CURSOR: &str = "backfill_cursor_before";
const COMMITTED_RECONCILIATION_CURSOR: &str = "committed_store_reconcile_cursor_before";
const PRUNE_CURSOR: &str = "prune_anchor_cursor_before";

impl SemanticVectorStore {
    fn required_state(connection: &Connection) -> Result<(SemanticSidecarStats, usize)> {
        let counts = connection
            .query_row(
                "SELECT embedded_items, embedded_chunks, dirty_items
                 FROM semantic_index_stats WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic vector store is missing its v7 cached counts",
                )
            })?;
        if counts.0 < 0 || counts.1 < counts.0 || counts.2 < 0 {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vector store has invalid v7 cached counts",
            )
            .into());
        }
        Ok((
            SemanticSidecarStats {
                embedded_items: counts.0 as usize,
                embedded_chunks: counts.1 as usize,
            },
            counts.2 as usize,
        ))
    }

    pub(super) fn cached_stats(&self) -> Result<Option<SemanticSidecarStats>> {
        semantic_owned_sidecar_result(Self::required_state(&self.conn).map(|state| Some(state.0)))
    }

    pub(super) fn cached_or_exact_stats(&self) -> Result<SemanticSidecarStats> {
        semantic_owned_sidecar_result(Self::required_state(&self.conn).map(|state| state.0))
    }

    pub(super) fn exact_stats(&self) -> Result<SemanticSidecarStats> {
        semantic_owned_sidecar_result((|| {
            let chunks =
                self.conn
                    .query_row("SELECT COUNT(*) FROM event_embedding_chunks", [], |row| {
                        row.get::<_, i64>(0)
                    })?;
            let items = self.conn.query_row(
                "SELECT COUNT(DISTINCT event_id) FROM event_embedding_chunks",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            if items < 0 || chunks < items {
                return Err(SemanticVectorStoreError::reset_required(
                    "semantic vector store returned invalid exact counts",
                )
                .into());
            }
            Ok(SemanticSidecarStats {
                embedded_items: items as usize,
                embedded_chunks: chunks as usize,
            })
        })())
    }

    fn apply_stats_delta(
        transaction: &rusqlite::Transaction<'_>,
        removed: SemanticSidecarStats,
        inserted: SemanticSidecarStats,
    ) -> Result<()> {
        let (current, _) = Self::required_state(transaction)?;
        let items = current
            .embedded_items
            .checked_sub(removed.embedded_items)
            .and_then(|value| value.checked_add(inserted.embedded_items));
        let chunks = current
            .embedded_chunks
            .checked_sub(removed.embedded_chunks)
            .and_then(|value| value.checked_add(inserted.embedded_chunks));
        let (Some(items), Some(chunks)) = (items, chunks) else {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vector cached-count delta overflowed",
            )
            .into());
        };
        if items > chunks {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vector cached item count exceeds chunk count",
            )
            .into());
        }
        let updated = transaction.execute(
            "UPDATE semantic_index_stats
             SET embedded_items = ?1, embedded_chunks = ?2 WHERE id = 1",
            params![i64::try_from(items)?, i64::try_from(chunks)?],
        )?;
        if updated != 1 {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vector store lost its v7 cached-count row",
            )
            .into());
        }
        Ok(())
    }

    fn apply_dirty_delta(
        transaction: &rusqlite::Transaction<'_>,
        removed: usize,
        inserted: usize,
    ) -> Result<()> {
        let (_, current) = Self::required_state(transaction)?;
        let dirty = current
            .checked_sub(removed)
            .and_then(|value| value.checked_add(inserted))
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic dirty cached-count delta overflowed",
                )
            })?;
        transaction.execute(
            "UPDATE semantic_index_stats SET dirty_items = ?1 WHERE id = 1",
            [i64::try_from(dirty)?],
        )?;
        Ok(())
    }

    fn event_stats(
        transaction: &rusqlite::Transaction<'_>,
        event_ids: &HashSet<Uuid>,
    ) -> Result<SemanticSidecarStats> {
        let mut statement = transaction
            .prepare("SELECT COUNT(*) FROM event_embedding_chunks WHERE event_id = ?1")?;
        let mut stats = SemanticSidecarStats::default();
        for event_id in event_ids {
            let chunks = statement.query_row([event_id.to_string()], |row| row.get::<_, i64>(0))?;
            let chunks = usize::try_from(chunks).map_err(|_| {
                SemanticVectorStoreError::reset_required(
                    "semantic vector store returned a negative event chunk count",
                )
            })?;
            stats.embedded_chunks = stats.embedded_chunks.checked_add(chunks).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic vector event chunk counts overflowed",
                )
            })?;
            stats.embedded_items += usize::from(chunks > 0);
        }
        Ok(stats)
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
        semantic_owned_sidecar_result(Self::required_state(&self.conn).map(|state| state.1))
    }

    pub(super) fn enqueue_dirty_documents(
        &mut self,
        documents: &[EventEmbeddingDocument],
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
        documents: &[EventEmbeddingDocument],
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
                        "invalid dirty event id in semantic vector store; manual rebuild required: {value}"
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
        semantic_owned_sidecar_result((|| {
            if event_ids.is_empty() {
                return Ok(HashMap::new());
            }
            let placeholders = (0..event_ids.len())
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT event_id, source_text_sha256 FROM event_embedding_chunks
                 WHERE event_id IN ({placeholders}) GROUP BY event_id, source_text_sha256"
            );
            let values = event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string()));
            let mut statement = self.conn.prepare(&sql)?;
            let mut rows = statement.query(params_from_iter(values))?;
            let mut hashes = HashMap::new();
            while let Some(row) = rows.next()? {
                let value = row.get::<_, String>(0)?;
                let event_id = Uuid::parse_str(&value).map_err(|_| {
                    SemanticVectorStoreError::reset_required(format!(
                        "invalid event id in semantic vector store; manual rebuild required: {value}"
                    ))
                })?;
                hashes.insert(event_id, row.get(1)?);
            }
            Ok(hashes)
        })())
    }

    pub(super) fn upsert_chunk_embeddings(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
    ) -> Result<()> {
        semantic_owned_sidecar_result((|| {
            if items.is_empty() {
                return Ok(());
            }
            if items
                .iter()
                .any(|(_, embedding)| embedding.len() != SEMANTIC_DIMENSIONS)
            {
                return Err(SemanticVectorStoreError::unavailable(format!(
                    "semantic embedding dimensions must equal {SEMANTIC_DIMENSIONS}"
                ))
                .into());
            }
            let event_ids = items
                .iter()
                .map(|(document, _)| document.event_id)
                .collect::<HashSet<_>>();
            let transaction = self.conn.transaction()?;
            let removed = Self::event_stats(&transaction, &event_ids)?;
            {
                let mut delete_vectors = transaction.prepare(
                    "DELETE FROM event_embedding_vec0 WHERE rowid IN (
                        SELECT chunk_id FROM event_embedding_chunks WHERE event_id = ?1
                     )",
                )?;
                let mut delete_metadata = transaction
                    .prepare("DELETE FROM event_embedding_chunks WHERE event_id = ?1")?;
                for event_id in &event_ids {
                    delete_vectors.execute([event_id.to_string()])?;
                    delete_metadata.execute([event_id.to_string()])?;
                }
            }
            {
                let mut insert_metadata = transaction.prepare(
                    "INSERT INTO event_embedding_chunks
                     (event_id, event_seq, chunk_index, source_text_sha256, start_char, end_char)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                let mut insert_vector = transaction.prepare(
                    "INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)",
                )?;
                for (document, embedding) in items {
                    insert_metadata.execute(params![
                        document.event_id.to_string(),
                        i64::try_from(document.seq)?,
                        i64::try_from(document.chunk_index)?,
                        document.source_text_hash,
                        i64::try_from(document.start_char)?,
                        i64::try_from(document.end_char)?,
                    ])?;
                    insert_vector.execute(params![
                        transaction.last_insert_rowid(),
                        serialize_f32_blob(embedding)
                    ])?;
                }
            }
            Self::apply_stats_delta(
                &transaction,
                removed,
                SemanticSidecarStats {
                    embedded_items: event_ids.len(),
                    embedded_chunks: items.len(),
                },
            )?;
            transaction.commit()?;
            Ok(())
        })())
    }

    pub(super) fn prune_ineligible_events(
        &mut self,
        store: &Store,
    ) -> Result<SemanticPruneOutcome> {
        let cursor = self.maintenance_cursor::<i64, Uuid>(PRUNE_CURSOR)?;
        let events = self.prune_candidates(cursor)?;
        if events.is_empty() {
            if cursor.is_some() {
                self.set_maintenance_cursor(PRUNE_CURSOR, None::<(i64, Uuid)>)?;
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
        for batch in events.chunks(SEMANTIC_PRUNE_EVENT_BATCH) {
            let ids = batch.iter().map(|event| event.0).collect::<Vec<_>>();
            let eligible = store.semantic_eligible_event_ids(&ids)?;
            let current = store
                .event_embedding_documents_by_ids(&ids)?
                .into_iter()
                .map(|document| (document.event_id, document))
                .collect::<HashMap<_, _>>();
            let mut delete = Vec::new();
            let mut stale = Vec::new();
            for (event_id, stored_hash, _) in batch {
                let Some(document) = current.get(event_id) else {
                    delete.push(*event_id);
                    continue;
                };
                if !eligible.contains(event_id) {
                    delete.push(*event_id);
                    continue;
                }
                let text = semantic_source_text(&document.text);
                if semantic_document_hash(document, &text) != *stored_hash {
                    delete.push(*event_id);
                    stale.push(document.clone());
                }
            }
            let transaction = self.conn.transaction()?;
            outcome.deleted_chunks += Self::delete_events_in_transaction(&transaction, &delete)?;
            outcome.queued_stale_events +=
                Self::enqueue_dirty_in_transaction(&transaction, &stale, "stale_hash")?;
            transaction.commit()?;
        }
        if outcome.scan_complete {
            self.set_maintenance_cursor(PRUNE_CURSOR, None::<(i64, Uuid)>)?;
        } else if let Some((event_id, _, seq)) = events.last() {
            self.set_maintenance_cursor(PRUNE_CURSOR, Some((*seq, *event_id)))?;
        }
        Ok(outcome)
    }

    fn prune_candidates(&self, cursor: Option<(i64, Uuid)>) -> Result<Vec<(Uuid, String, i64)>> {
        semantic_owned_sidecar_result((|| {
            let sql = if cursor.is_some() {
                "SELECT event_id, source_text_sha256, event_seq
                 FROM event_embedding_chunks WHERE chunk_index = 0
                   AND (event_seq, event_id) < (?1, ?2)
                 ORDER BY event_seq DESC, event_id DESC LIMIT ?3"
            } else {
                "SELECT event_id, source_text_sha256, event_seq
                 FROM event_embedding_chunks WHERE chunk_index = 0
                 ORDER BY event_seq DESC, event_id DESC LIMIT ?1"
            };
            let mut statement = self.conn.prepare(sql)?;
            let mut rows = match cursor {
                Some((seq, event_id)) => statement.query(params![
                    seq,
                    event_id.to_string(),
                    i64::try_from(SEMANTIC_PRUNE_EVENTS_PER_PASS)?
                ])?,
                None => statement.query([i64::try_from(SEMANTIC_PRUNE_EVENTS_PER_PASS)?])?,
            };
            let mut events = Vec::new();
            while let Some(row) = rows.next()? {
                let value = row.get::<_, String>(0)?;
                let event_id = Uuid::parse_str(&value).map_err(|_| {
                    SemanticVectorStoreError::reset_required(format!(
                        "invalid prune event id in semantic vector store; manual rebuild required: {value}"
                    ))
                })?;
                events.push((event_id, row.get(1)?, row.get(2)?));
            }
            Ok(events)
        })())
    }

    pub(super) fn delete_events_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        event_ids: &[Uuid],
    ) -> Result<usize> {
        let ids = event_ids.iter().copied().collect::<HashSet<_>>();
        let removed = Self::event_stats(transaction, &ids)?;
        let mut deleted = 0;
        {
            let mut delete_vectors = transaction.prepare(
                "DELETE FROM event_embedding_vec0 WHERE rowid IN (
                    SELECT chunk_id FROM event_embedding_chunks WHERE event_id = ?1
                 )",
            )?;
            let mut delete_metadata =
                transaction.prepare("DELETE FROM event_embedding_chunks WHERE event_id = ?1")?;
            let mut delete_source_metadata =
                transaction.prepare("DELETE FROM semantic_source_documents WHERE event_id = ?1")?;
            for event_id in ids {
                delete_vectors.execute([event_id.to_string()])?;
                deleted += delete_metadata.execute([event_id.to_string()])?;
                delete_source_metadata.execute([event_id.to_string()])?;
            }
        }
        if deleted != removed.embedded_chunks {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vector metadata changed during delete",
            )
            .into());
        }
        Self::apply_stats_delta(transaction, removed, SemanticSidecarStats::default())?;
        Ok(deleted)
    }

    #[cfg(test)]
    pub(super) fn delete_embedding_chunks_for_event_ids(
        &mut self,
        event_ids: &[Uuid],
    ) -> Result<usize> {
        semantic_owned_sidecar_result((|| {
            let transaction = self.conn.transaction()?;
            let deleted = Self::delete_events_in_transaction(&transaction, event_ids)?;
            transaction.commit()?;
            Ok(deleted)
        })())
    }
}
