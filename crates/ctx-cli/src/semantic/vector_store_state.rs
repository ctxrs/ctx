impl SemanticVectorStore {
    fn cached_stats(&self) -> Result<Option<SemanticSidecarStats>> {
        if !sqlite_table_exists(&self.conn, "semantic_index_stats")? {
            return Ok(None);
        }
        let stats = self
            .conn
            .query_row(
                r#"
                SELECT embedded_items, embedded_chunks
                FROM semantic_index_stats
                WHERE model_key = ?1
                "#,
                params![SEMANTIC_MODEL_KEY],
                |row| {
                    let embedded_items = row.get::<_, i64>(0)?.max(0) as usize;
                    let embedded_chunks = row.get::<_, i64>(1)?.max(0) as usize;
                    Ok(SemanticSidecarStats {
                        embedded_items,
                        embedded_chunks,
                    })
                },
            )
            .optional()?;
        Ok(stats)
    }

    fn exact_stats(&self) -> Result<SemanticSidecarStats> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticSidecarStats::default());
        }
        let embedded_chunks = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        let embedded_items = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT event_id) FROM event_embedding_chunks WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(SemanticSidecarStats {
            embedded_items: embedded_items.max(0) as usize,
            embedded_chunks: embedded_chunks.max(0) as usize,
        })
    }

    fn cached_or_exact_stats(&self) -> Result<SemanticSidecarStats> {
        if let Some(stats) = self.cached_stats()? {
            return Ok(stats);
        }
        self.exact_stats()
    }

    fn refresh_cached_stats(&self) -> Result<SemanticSidecarStats> {
        let stats = self.exact_stats()?;
        self.conn.execute(
            r#"
            INSERT INTO semantic_index_stats
                (model_key, embedded_items, embedded_chunks, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(model_key) DO UPDATE SET
                embedded_items = excluded.embedded_items,
                embedded_chunks = excluded.embedded_chunks,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                SEMANTIC_MODEL_KEY,
                stats.embedded_items as i64,
                stats.embedded_chunks as i64,
                utc_now().timestamp_millis()
            ],
        )?;
        Ok(stats)
    }

    fn maintenance_state_i64(&self, key: &str) -> Result<Option<i64>> {
        if !sqlite_table_exists(&self.conn, "semantic_maintenance_state")? {
            return Ok(None);
        }
        let value = self
            .conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.and_then(|value| value.parse::<i64>().ok()))
    }

    fn set_maintenance_state_i64(&self, key: &str, value: i64) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO semantic_maintenance_state (key, value, updated_at_ms)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![key, value.to_string(), utc_now().timestamp_millis()],
        )?;
        Ok(())
    }

    fn delete_maintenance_state_keys(&self, keys: &[&str]) -> Result<()> {
        if keys.is_empty() || !sqlite_table_exists(&self.conn, "semantic_maintenance_state")? {
            return Ok(());
        }
        for key in keys {
            self.conn
                .execute("DELETE FROM semantic_maintenance_state WHERE key = ?1", [key])?;
        }
        Ok(())
    }

    fn backfill_cursor(&self) -> Result<Option<(i64, u64)>> {
        let Some(occurred_at_ms) = self.maintenance_state_i64("backfill_occurred_at_ms_before")?
        else {
            return Ok(None);
        };
        let Some(seq) = self.maintenance_state_i64("backfill_seq_before")? else {
            return Ok(None);
        };
        Ok(Some((occurred_at_ms, seq.max(0) as u64)))
    }

    fn set_backfill_cursor(&self, cursor: Option<(i64, u64)>) -> Result<()> {
        match cursor {
            Some((occurred_at_ms, seq)) => {
                self.set_maintenance_state_i64("backfill_occurred_at_ms_before", occurred_at_ms)?;
                self.set_maintenance_state_i64("backfill_seq_before", seq as i64)?;
            }
            None => self.delete_maintenance_state_keys(&[
                "backfill_occurred_at_ms_before",
                "backfill_seq_before",
            ])?,
        }
        Ok(())
    }

    fn dirty_event_count(&self) -> Result<usize> {
        if !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(0);
        }
        let count = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM semantic_dirty_events WHERE model_key = ?1",
                params![SEMANTIC_MODEL_KEY],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(count.max(0) as usize)
    }

    fn enqueue_dirty_documents(
        &mut self,
        docs: &[EventEmbeddingDocument],
        reason: &str,
    ) -> Result<usize> {
        if docs.is_empty() {
            return Ok(0);
        }
        let reason = reason.chars().take(64).collect::<String>();
        let queued_at_ms = utc_now().timestamp_millis();
        let tx = self.conn.transaction()?;
        let mut changed = 0_usize;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO semantic_dirty_events
                    (event_id, model_key, queued_at_ms, priority_seq, reason, attempts)
                VALUES (?1, ?2, ?3, ?4, ?5, 0)
                ON CONFLICT(event_id, model_key) DO UPDATE SET
                    queued_at_ms = excluded.queued_at_ms,
                    priority_seq = COALESCE(excluded.priority_seq, semantic_dirty_events.priority_seq),
                    reason = excluded.reason
                "#,
            )?;
            for doc in docs {
                changed = changed.saturating_add(stmt.execute(params![
                    doc.event_id.to_string(),
                    SEMANTIC_MODEL_KEY,
                    queued_at_ms,
                    doc.seq as i64,
                    reason
                ])?);
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    fn queued_dirty_event_ids(&self, limit: usize) -> Result<Vec<Uuid>> {
        if limit == 0 || !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_id
            FROM semantic_dirty_events
            WHERE model_key = ?1
            ORDER BY priority_seq IS NULL, priority_seq DESC, queued_at_ms ASC
            LIMIT ?2
            "#,
        )?;
        let mut rows = stmt.query(params![SEMANTIC_MODEL_KEY, limit as i64])?;
        let mut event_ids = Vec::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            let event_id = Uuid::parse_str(&event_id_text)
                .context("invalid dirty event id in semantic vector store")?;
            event_ids.push(event_id);
        }
        Ok(event_ids)
    }

    fn dequeue_dirty_events(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut deleted = 0_usize;
        {
            let mut stmt = tx.prepare(
                "DELETE FROM semantic_dirty_events WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![SEMANTIC_MODEL_KEY, event_id.to_string()])?,
                );
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    fn plaintext_value_count(&self) -> Result<usize> {
        let mut count = 0_usize;
        if sqlite_column_exists(&self.conn, "event_embeddings", "preview_text")? {
            let rows = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM event_embeddings WHERE preview_text != ''",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            count = count.saturating_add(rows.max(0) as usize);
        }
        if sqlite_column_exists(&self.conn, "event_embedding_chunks", "chunk_text")? {
            let rows = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM event_embedding_chunks WHERE chunk_text != ''",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .unwrap_or(0);
            count = count.saturating_add(rows.max(0) as usize);
        }
        Ok(count)
    }

    fn existing_hashes_for_event_ids(&self, event_ids: &[Uuid]) -> Result<HashMap<Uuid, String>> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(HashMap::new());
        }
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT event_id, source_text_sha256
            FROM event_embedding_chunks
            WHERE model_key = ?
              AND event_id IN ({placeholders})
            GROUP BY event_id, source_text_sha256
            "#
        );
        let mut query_params = vec![SqlValue::from(SEMANTIC_MODEL_KEY.to_owned())];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut hashes = HashMap::new();
        while let Some(row) = rows.next()? {
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            hashes.insert(event_id, row.get(1)?);
        }
        Ok(hashes)
    }

    fn upsert_chunk_embeddings(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let maintain_sqlite_vec0 = self.sqlite_vec0_runtime_available()
            && sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            && sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?;
        let tx = self.conn.transaction()?;
        {
            if maintain_sqlite_vec0 {
                let mut delete_vec_stmt = tx.prepare(
                    r#"
                    DELETE FROM event_embedding_vec0
                    WHERE rowid IN (
                        SELECT rowid
                        FROM event_embedding_vec0_meta
                        WHERE model_key = ?1 AND event_id = ?2
                    )
                    "#,
                )?;
                let mut delete_meta_stmt = tx.prepare(
                    "DELETE FROM event_embedding_vec0_meta WHERE model_key = ?1 AND event_id = ?2",
                )?;
                let mut deleted_events = std::collections::HashSet::new();
                for (doc, _) in items {
                    if deleted_events.insert(doc.event_id) {
                        let event_id = doc.event_id.to_string();
                        delete_vec_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                        delete_meta_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                    }
                }
            }
            let mut delete_stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            )?;
            let mut deleted_events = std::collections::HashSet::new();
            for (doc, _) in items {
                if deleted_events.insert(doc.event_id) {
                    delete_stmt.execute(params![doc.event_id.to_string(), SEMANTIC_MODEL_KEY])?;
                }
            }
            drop(delete_stmt);

            let mut stmt = tx.prepare(
                r#"
                INSERT INTO event_embedding_chunks
                    (event_id, model_key, history_record_id, session_id, event_seq,
                     chunk_index, chunk_count, source_text_sha256, chunk_text_sha256,
                     chunk_text, start_char, end_char, dimensions, embedding_f32, embedded_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
            )?;
            let mut vec0_meta_stmt = if maintain_sqlite_vec0 {
                Some(tx.prepare(
                    r#"
	                    INSERT INTO event_embedding_vec0_meta
	                        (rowid, event_id, model_key, history_record_id, session_id, event_seq,
	                         chunk_index, source_text_sha256, start_char, end_char)
	                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
	                    "#,
                )?)
            } else {
                None
            };
            let mut vec0_stmt = if maintain_sqlite_vec0 {
                Some(tx.prepare(
                    "INSERT INTO event_embedding_vec0(rowid, embedding) VALUES (?1, ?2)",
                )?)
            } else {
                None
            };
            let embedded_at_ms = utc_now().timestamp_millis();
            for (doc, embedding) in items {
                let event_id = doc.event_id.to_string();
                let history_record_id = doc.history_record_id.map(|id| id.to_string());
                let session_id = doc.session_id.map(|id| id.to_string());
                let blob = serialize_f32_blob(embedding);
                stmt.execute(params![
                    &event_id,
                    SEMANTIC_MODEL_KEY,
                    &history_record_id,
                    &session_id,
                    doc.seq as i64,
                    doc.chunk_index as i64,
                    doc.chunk_count as i64,
                    doc.source_text_hash,
                    doc.chunk_text_hash,
                    "",
                    doc.start_char as i64,
                    doc.end_char as i64,
                    SEMANTIC_DIMENSIONS as i64,
                    &blob,
                    embedded_at_ms
                ])?;
                let rowid = tx.last_insert_rowid();
                if let (Some(meta_stmt), Some(vec_stmt)) =
                    (vec0_meta_stmt.as_mut(), vec0_stmt.as_mut())
                {
                    meta_stmt.execute(params![
                        rowid,
                        &event_id,
                        SEMANTIC_MODEL_KEY,
                        &history_record_id,
                        &session_id,
                        doc.seq as i64,
                        doc.chunk_index as i64,
                        &doc.source_text_hash,
                        doc.start_char as i64,
                        doc.end_char as i64,
                    ])?;
                    vec_stmt.execute(params![rowid, &blob])?;
                }
            }
        }
        tx.commit()?;
        self.refresh_cached_stats()?;
        Ok(())
    }

    fn prune_ineligible_events(&mut self, store: &Store) -> Result<SemanticPruneOutcome> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticPruneOutcome::default());
        }
        let mut sidecar_events =
            self.prune_candidate_events(self.maintenance_state_i64("prune_event_seq_before")?)?;
        if sidecar_events.is_empty()
            && self
                .maintenance_state_i64("prune_event_seq_before")?
                .is_some()
        {
            sidecar_events = self.prune_candidate_events(None)?;
        }

        let next_cursor = sidecar_events.last().map(|(_, _, _, event_seq)| *event_seq);
        let mut outcome = SemanticPruneOutcome::default();
        for chunk in sidecar_events.chunks(SEMANTIC_PRUNE_EVENT_BATCH) {
            let event_ids = chunk
                .iter()
                .map(|(event_id, _, _, _)| *event_id)
                .collect::<Vec<_>>();
            let eligible_event_ids = store.semantic_eligible_event_ids(&event_ids)?;
            let current_docs = store.event_embedding_documents_by_ids(&event_ids)?;
            let current_by_id = current_docs
                .into_iter()
                .map(|doc| (doc.event_id, doc))
                .collect::<HashMap<_, _>>();
            let mut delete_event_ids = Vec::new();
            let mut stale_docs = Vec::new();
            for (event_id, stored_hash, single_hash, _) in chunk {
                let Some(doc) = current_by_id.get(event_id) else {
                    delete_event_ids.push(*event_id);
                    continue;
                };
                if !eligible_event_ids.contains(event_id) {
                    delete_event_ids.push(*event_id);
                    continue;
                }
                let source_text = semantic_source_text(&doc.text);
                let current_hash = semantic_document_hash(doc, &source_text);
                if !*single_hash || current_hash != *stored_hash {
                    delete_event_ids.push(*event_id);
                    stale_docs.push(doc.clone());
                }
            }
            outcome.deleted_chunks = outcome
                .deleted_chunks
                .saturating_add(self.delete_embedding_chunks_for_event_ids(&delete_event_ids)?);
            if !stale_docs.is_empty() {
                outcome.queued_stale_events = outcome
                    .queued_stale_events
                    .saturating_add(self.enqueue_dirty_documents(&stale_docs, "stale_hash")?);
            }
        }
        if let Some(next_cursor) = next_cursor {
            self.set_maintenance_state_i64("prune_event_seq_before", next_cursor)?;
        }
        self.refresh_cached_stats()?;
        Ok(outcome)
    }

    fn prune_candidate_events(
        &self,
        before_event_seq: Option<i64>,
    ) -> Result<Vec<(Uuid, String, bool, i64)>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_id,
                   MIN(source_text_sha256),
                   COUNT(DISTINCT source_text_sha256),
                   MAX(event_seq)
            FROM event_embedding_chunks
            WHERE model_key = ?1
              AND (?2 IS NULL OR event_seq < ?2)
            GROUP BY event_id
            ORDER BY MAX(event_seq) DESC
            LIMIT ?3
            "#,
        )?;
        let mut rows = stmt.query(params![
            SEMANTIC_MODEL_KEY,
            before_event_seq,
            SEMANTIC_PRUNE_EVENTS_PER_PASS as i64
        ])?;
        let mut sidecar_events = Vec::<(Uuid, String, bool, i64)>::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            if let Ok(event_id) = Uuid::parse_str(&event_id_text) {
                let source_text_hash = row.get::<_, String>(1)?;
                let hash_versions = row.get::<_, i64>(2)?.max(0);
                let event_seq = row.get::<_, i64>(3)?.max(0);
                sidecar_events.push((event_id, source_text_hash, hash_versions == 1, event_seq));
            }
        }
        Ok(sidecar_events)
    }

    fn delete_embedding_chunks_for_event_ids(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(0);
        }
        let maintain_sqlite_vec0 = self.sqlite_vec0_runtime_available()
            && sqlite_table_exists(&self.conn, "event_embedding_vec0")?
            && sqlite_table_exists(&self.conn, "event_embedding_vec0_meta")?;
        let tx = self.conn.transaction()?;
        let mut deleted = 0_usize;
        {
            if maintain_sqlite_vec0 {
                let mut delete_vec_stmt = tx.prepare(
                    r#"
                    DELETE FROM event_embedding_vec0
                    WHERE rowid IN (
                        SELECT rowid
                        FROM event_embedding_vec0_meta
                        WHERE model_key = ?1 AND event_id = ?2
                    )
                    "#,
                )?;
                let mut delete_meta_stmt = tx.prepare(
                    "DELETE FROM event_embedding_vec0_meta WHERE model_key = ?1 AND event_id = ?2",
                )?;
                for event_id in event_ids {
                    let event_id = event_id.to_string();
                    delete_vec_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                    delete_meta_stmt.execute(params![SEMANTIC_MODEL_KEY, &event_id])?;
                }
            }
            let mut stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![SEMANTIC_MODEL_KEY, event_id.to_string()])?,
                );
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

}
