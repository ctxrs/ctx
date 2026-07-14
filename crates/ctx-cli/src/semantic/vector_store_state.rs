impl SemanticVectorStore {
    fn maintenance_state_key(key: &str) -> String {
        format!("{}:{key}", semantic_model_key())
    }

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
                params![semantic_model_key()],
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
        let embedded_chunks = self.exact_chunk_count()?;
        let embedded_items = self
            .conn
            .query_row(
                "SELECT COUNT(DISTINCT event_id) FROM event_embedding_chunks WHERE model_key = ?1",
                params![semantic_model_key()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(SemanticSidecarStats {
            embedded_items: embedded_items.max(0) as usize,
            embedded_chunks,
        })
    }

    fn exact_chunk_count(&self) -> Result<usize> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(0);
        }
        let embedded_chunks = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_embedding_chunks WHERE model_key = ?1",
                params![semantic_model_key()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(embedded_chunks.max(0) as usize)
    }

    fn cached_or_exact_stats(&self) -> Result<SemanticSidecarStats> {
        if let Some(stats) = self.cached_stats()? {
            return Ok(stats);
        }
        self.exact_stats()
    }

    fn maintenance_state_i64(&self, key: &str) -> Result<Option<i64>> {
        if !sqlite_table_exists(&self.conn, "semantic_maintenance_state")? {
            return Ok(None);
        }
        let key = Self::maintenance_state_key(key);
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
        let key = Self::maintenance_state_key(key);
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

    fn set_maintenance_state_i64_admitted(
        &self,
        key: &str,
        value: i64,
        operation: &'static str,
    ) -> Result<()> {
        let _lease = self.acquire_write_lease(4096, operation)?;
        self.set_maintenance_state_i64(key, value)
    }

    fn delete_maintenance_state_keys(&self, keys: &[&str]) -> Result<()> {
        if keys.is_empty() || !sqlite_table_exists(&self.conn, "semantic_maintenance_state")? {
            return Ok(());
        }
        for key in keys {
            let key = Self::maintenance_state_key(key);
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
        let _lease = self.acquire_write_lease(4096, "semantic backfill checkpoint")?;
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
                params![semantic_model_key()],
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
        let lease = self.acquire_write_lease(
            (docs.len() as u64).saturating_mul(4096),
            "semantic dirty queue update",
        )?;
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
                    semantic_model_key(),
                    queued_at_ms,
                    doc.seq as i64,
                    reason
                ])?);
            }
        }
        commit_semantic_transaction(tx, || {
            Ok(lease.revalidate_growth("semantic dirty queue update")?)
        })?;
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
        let mut rows = stmt.query(params![semantic_model_key(), limit as i64])?;
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
        let lease = self.acquire_write_lease(
            (event_ids.len() as u64).saturating_mul(1024),
            "semantic dirty queue dequeue",
        )?;
        let tx = self.conn.transaction()?;
        let mut deleted = 0_usize;
        {
            let mut stmt = tx.prepare(
                "DELETE FROM semantic_dirty_events WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![semantic_model_key(), event_id.to_string()])?,
                );
            }
        }
        commit_semantic_transaction(tx, || {
            Ok(lease.revalidate_growth("semantic dirty queue dequeue")?)
        })?;
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
        let mut query_params = vec![SqlValue::from(semantic_model_key().to_owned())];
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
        let estimated_write_bytes = items.iter().fold(0_u64, |total, (_, embedding)| {
            total.saturating_add((embedding.len() as u64).saturating_mul(8).saturating_add(4096))
        });
        let lease = self.acquire_write_lease(
            estimated_write_bytes.max(INDEXING_WAL_DELTA_BYTES),
            "semantic vector upsert",
        )?;
        let vec0_tables = self.vec0_table_pairs_for_write()?;
        let event_ids = items
            .iter()
            .map(|(doc, _)| doc.event_id)
            .collect::<HashSet<_>>();
        let tx = self.conn.transaction()?;
        let (old_items, old_chunks) = semantic_existing_counts_for_events(&tx, &event_ids)?;
        {
            for (vec_table, meta_table) in &vec0_tables {
                for event_id in &event_ids {
                    tx.execute(
                        &format!(
                            "DELETE FROM {vec_table} WHERE rowid IN (SELECT rowid FROM {meta_table} WHERE model_key = ?1 AND event_id = ?2)"
                        ),
                        params![semantic_model_key(), event_id.to_string()],
                    )?;
                    tx.execute(
                        &format!(
                            "DELETE FROM {meta_table} WHERE model_key = ?1 AND event_id = ?2"
                        ),
                        params![semantic_model_key(), event_id.to_string()],
                    )?;
                }
            }
            let mut delete_stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            )?;
            let mut deleted_events = std::collections::HashSet::new();
            for (doc, _) in items {
                if deleted_events.insert(doc.event_id) {
                    delete_stmt.execute(params![doc.event_id.to_string(), semantic_model_key()])?;
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
            let embedded_at_ms = utc_now().timestamp_millis();
            for (doc, embedding) in items {
                let event_id = doc.event_id.to_string();
                let history_record_id = doc.history_record_id.map(|id| id.to_string());
                let session_id = doc.session_id.map(|id| id.to_string());
                let blob = serialize_f32_blob(embedding);
                stmt.execute(params![
                    &event_id,
                    semantic_model_key(),
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
                for (vec_table, meta_table) in &vec0_tables {
                    tx.execute(
                        &format!(
                            "INSERT INTO {meta_table} (rowid, event_id, model_key, history_record_id, session_id, event_seq, chunk_index, source_text_sha256, start_char, end_char) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
                        ),
                        params![
                        rowid,
                        &event_id,
                        semantic_model_key(),
                        &history_record_id,
                        &session_id,
                        doc.seq as i64,
                        doc.chunk_index as i64,
                        &doc.source_text_hash,
                        doc.start_char as i64,
                        doc.end_char as i64,
                        ],
                    )?;
                    tx.execute(
                        &format!(
                            "INSERT INTO {vec_table}(rowid, embedding) VALUES (?1, ?2)"
                        ),
                        params![rowid, &blob],
                    )?;
                }
            }
            update_semantic_cached_stats_delta(
                &tx,
                items.len() as i64 - old_chunks as i64,
                event_ids.len() as i64 - old_items as i64,
            )?;
        }
        commit_semantic_transaction(tx, || {
            Ok(lease.revalidate_growth("semantic vector upsert")?)
        })?;
        Ok(())
    }

    fn vec0_table_pairs_for_write(&self) -> Result<Vec<(&'static str, &'static str)>> {
        if !self.sqlite_vec0_runtime_available() {
            return Ok(Vec::new());
        }
        let mut tables = Vec::new();
        let active_generation = self.maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?;
        if let Some((vec_table, meta_table)) =
            active_generation.and_then(sqlite_vec0_generation_tables)
        {
            if sqlite_table_exists(&self.conn, vec_table)?
                && sqlite_table_exists(&self.conn, meta_table)?
                && self.sqlite_vec0_schema_compatible()?
            {
                tables.push((vec_table, meta_table));
            }
        }
        if matches!(
            self.maintenance_state_i64(SEMANTIC_VEC0_REBUILD_STAGE_KEY)?,
            Some(2 | 3)
        ) {
            let rebuild_generation = self
                .maintenance_state_i64(SEMANTIC_VEC0_REBUILD_GENERATION_KEY)?;
            if rebuild_generation != active_generation {
                if let Some((vec_table, meta_table)) =
                    rebuild_generation.and_then(sqlite_vec0_generation_tables)
                {
                    if sqlite_table_exists(&self.conn, vec_table)?
                        && sqlite_table_exists(&self.conn, meta_table)?
                    {
                        tables.push((vec_table, meta_table));
                    }
                }
            }
        }
        Ok(tables)
    }

    fn prune_ineligible_events(&mut self, store: &Store) -> Result<SemanticPruneOutcome> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticPruneOutcome::default());
        }
        let relational_revision = store.semantic_content_revision()?;
        if self.validated_relational_revision()? == Some(relational_revision) {
            return Ok(SemanticPruneOutcome {
                validation_complete: true,
                target_revision: relational_revision,
                ..SemanticPruneOutcome::default()
            });
        }
        let mut target_revision = self
            .maintenance_state_i64("validation_target_revision")?
            .map(|value| value.max(0) as u64);
        if target_revision != Some(relational_revision) {
            self.reset_relational_validation(relational_revision)?;
            target_revision = Some(relational_revision);
        }
        let cursor = self
            .maintenance_state_i64("validation_sidecar_rowid_before")?
            .unwrap_or(i64::MAX);
        let sidecar_events = self.prune_candidate_events(cursor)?;

        let next_cursor = sidecar_events.last().map(|(_, _, _, rowid)| *rowid);
        let mut outcome = SemanticPruneOutcome {
            target_revision: target_revision.unwrap_or(relational_revision),
            validation_advanced: true,
            ..SemanticPruneOutcome::default()
        };
        if sidecar_events.is_empty() {
            let current_revision = store.semantic_content_revision()?;
            if current_revision == outcome.target_revision {
                self.finish_relational_validation(current_revision)?;
                outcome.validation_complete = true;
            } else {
                self.reset_relational_validation(current_revision)?;
                outcome.target_revision = current_revision;
            }
            return Ok(outcome);
        }
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
            self.set_maintenance_state_i64_admitted(
                "validation_sidecar_rowid_before",
                next_cursor,
                "semantic validation checkpoint",
            )?;
        }
        Ok(outcome)
    }

    fn prune_candidate_events(
        &self,
        before_rowid: i64,
    ) -> Result<Vec<(Uuid, String, bool, i64)>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT event_id,
                   MIN(source_text_sha256),
                   COUNT(DISTINCT source_text_sha256),
                   MAX(rowid)
            FROM event_embedding_chunks
            WHERE model_key = ?1
              AND rowid < ?2
            GROUP BY event_id
            ORDER BY MAX(rowid) DESC
            LIMIT ?3
            "#,
        )?;
        let mut rows = stmt.query(params![
            semantic_model_key(),
            before_rowid,
            SEMANTIC_PRUNE_EVENTS_PER_PASS as i64
        ])?;
        let mut sidecar_events = Vec::<(Uuid, String, bool, i64)>::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            if let Ok(event_id) = Uuid::parse_str(&event_id_text) {
                let source_text_hash = row.get::<_, String>(1)?;
                let hash_versions = row.get::<_, i64>(2)?.max(0);
                let rowid = row.get::<_, i64>(3)?.max(0);
                sidecar_events.push((event_id, source_text_hash, hash_versions == 1, rowid));
            }
        }
        Ok(sidecar_events)
    }

    fn validated_relational_revision(&self) -> Result<Option<u64>> {
        Ok(self
            .maintenance_state_i64("validated_relational_revision")?
            .map(|value| value.max(0) as u64))
    }

    fn relational_validation_is_current(&self, store: &Store) -> Result<bool> {
        Ok(self.validated_relational_revision()? == Some(store.semantic_content_revision()?))
    }

    fn reset_relational_validation(&mut self, revision: u64) -> Result<()> {
        let lease = self.acquire_write_lease(4096, "semantic validation reset")?;
        let tx = self.conn.transaction()?;
        set_semantic_state_i64_conn(
            &tx,
            "validation_target_revision",
            revision.min(i64::MAX as u64) as i64,
        )?;
        set_semantic_state_i64_conn(&tx, "validation_sidecar_rowid_before", i64::MAX)?;
        delete_semantic_state_conn(&tx, "validated_relational_revision")?;
        commit_semantic_transaction(tx, || {
            Ok(lease.revalidate_growth("semantic validation reset")?)
        })?;
        Ok(())
    }

    fn finish_relational_validation(&mut self, revision: u64) -> Result<()> {
        let lease = self.acquire_write_lease(4096, "semantic validation completion")?;
        let tx = self.conn.transaction()?;
        set_semantic_state_i64_conn(
            &tx,
            "validated_relational_revision",
            revision.min(i64::MAX as u64) as i64,
        )?;
        delete_semantic_state_conn(&tx, "validation_target_revision")?;
        delete_semantic_state_conn(&tx, "validation_sidecar_rowid_before")?;
        commit_semantic_transaction(tx, || {
            Ok(lease.revalidate_growth("semantic validation completion")?)
        })?;
        Ok(())
    }

    fn delete_embedding_chunks_for_event_ids(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(0);
        }
        let lease = self.acquire_write_lease(
            (event_ids.len() as u64)
                .saturating_mul(4096)
                .max(INDEXING_WAL_DELTA_BYTES),
            "semantic vector prune",
        )?;
        let vec0_tables = self.vec0_table_pairs_for_write()?;
        let event_id_set = event_ids.iter().copied().collect::<HashSet<_>>();
        let tx = self.conn.transaction()?;
        let (old_items, old_chunks) = semantic_existing_counts_for_events(&tx, &event_id_set)?;
        let mut deleted = 0_usize;
        {
            for (vec_table, meta_table) in &vec0_tables {
                for event_id in event_ids {
                    let event_id = event_id.to_string();
                    tx.execute(
                        &format!(
                            "DELETE FROM {vec_table} WHERE rowid IN (SELECT rowid FROM {meta_table} WHERE model_key = ?1 AND event_id = ?2)"
                        ),
                        params![semantic_model_key(), &event_id],
                    )?;
                    tx.execute(
                        &format!(
                            "DELETE FROM {meta_table} WHERE model_key = ?1 AND event_id = ?2"
                        ),
                        params![semantic_model_key(), &event_id],
                    )?;
                }
            }
            let mut stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE model_key = ?1 AND event_id = ?2",
            )?;
            for event_id in event_ids {
                deleted = deleted.saturating_add(
                    stmt.execute(params![semantic_model_key(), event_id.to_string()])?,
                );
            }
            update_semantic_cached_stats_delta(
                &tx,
                -(old_chunks as i64),
                -(old_items as i64),
            )?;
        }
        commit_semantic_transaction(tx, || {
            Ok(lease.revalidate_growth("semantic vector prune")?)
        })?;
        Ok(deleted)
    }

}

fn commit_semantic_transaction(
    tx: rusqlite::Transaction<'_>,
    revalidate: impl FnOnce() -> Result<()>,
) -> Result<()> {
    revalidate()?;
    tx.commit()?;
    Ok(())
}

fn semantic_existing_counts_for_events(
    conn: &Connection,
    event_ids: &HashSet<Uuid>,
) -> Result<(usize, usize)> {
    if event_ids.is_empty() {
        return Ok((0, 0));
    }
    let placeholders = (0..event_ids.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT COUNT(DISTINCT event_id), COUNT(*) FROM event_embedding_chunks WHERE model_key = ? AND event_id IN ({placeholders})"
    );
    let mut values = vec![SqlValue::from(semantic_model_key().to_owned())];
    values.extend(
        event_ids
            .iter()
            .map(|event_id| SqlValue::from(event_id.to_string())),
    );
    let (items, chunks) = conn.query_row(&sql, params_from_iter(values), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok((items.max(0) as usize, chunks.max(0) as usize))
}

fn update_semantic_cached_stats_delta(
    conn: &Connection,
    chunk_delta: i64,
    item_delta: i64,
) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO semantic_index_stats
            (model_key, embedded_items, embedded_chunks, updated_at_ms)
        VALUES (?1, MAX(?2, 0), MAX(?3, 0), ?4)
        ON CONFLICT(model_key) DO UPDATE SET
            embedded_items = MAX(semantic_index_stats.embedded_items + ?2, 0),
            embedded_chunks = MAX(semantic_index_stats.embedded_chunks + ?3, 0),
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![
            semantic_model_key(),
            item_delta,
            chunk_delta,
            utc_now().timestamp_millis(),
        ],
    )?;
    Ok(())
}
