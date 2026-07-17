impl SemanticVectorStore {
    fn maintenance_state_key(key: &str) -> String {
        format!("{}:{key}", semantic_model_key())
    }

    fn cached_stats(&self) -> Result<Option<SemanticSidecarStats>> {
        if !self.stats_and_summary_trusted()? {
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

    fn stats_and_summary_trusted(&self) -> Result<bool> {
        Self::stats_and_summary_trusted_on_connection(&self.conn)
    }

    fn stats_and_summary_trusted_on_connection(conn: &Connection) -> Result<bool> {
        if !sqlite_table_exists(conn, "semantic_index_stats")?
            || !sqlite_table_exists(conn, "semantic_event_summary")?
            || !sqlite_table_exists(conn, "semantic_maintenance_state")?
            || !Self::active_model_tuple_matches_on_connection(conn)?
        {
            return Ok(false);
        }
        let canonical_generation =
            Self::maintenance_state_i64_on_connection(conn, CANONICAL_GENERATION_STATE_KEY)?;
        let summary_generation =
            Self::maintenance_state_i64_on_connection(conn, SUMMARY_GENERATION_STATE_KEY)?;
        let summary_slot =
            Self::maintenance_state_i64_on_connection(conn, SUMMARY_ACTIVE_SLOT_STATE_KEY)?;
        let sanitized = Self::global_maintenance_state_i64_on_connection(
            conn,
            PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY,
        )?;
        let stats = conn
            .query_row(
                r#"
                SELECT embedded_items, embedded_chunks, trust_version, generation
                FROM semantic_index_stats
                WHERE model_key = ?1
                "#,
                [semantic_model_key()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(
            matches!(canonical_generation, Some(generation) if generation > 0)
                && summary_generation == canonical_generation
                && matches!(summary_slot, Some(0 | 1))
                && sanitized == Some(PLAINTEXT_SANITIZED_STATE_VERSION)
                && stats.is_some_and(|(items, chunks, trust, generation)| {
                    items >= 0
                        && chunks >= 0
                        && trust == SEMANTIC_SIDECAR_TRUST_VERSION
                        && Some(generation) == canonical_generation
                }),
        )
    }

    fn canonical_mutation_state_in_transaction(
        tx: &rusqlite::Transaction<'_>,
    ) -> Result<(i64, bool, Option<i64>, Option<i64>)> {
        if !Self::active_model_tuple_matches_on_connection(tx)? {
            return Err(SemanticVectorStoreTerminal::new("active model tuple mismatch").into());
        }
        let canonical_generation =
            Self::maintenance_state_i64_in_transaction(tx, CANONICAL_GENERATION_STATE_KEY)?
                .unwrap_or(0);
        let stats_trusted = Self::stats_and_summary_trusted_on_connection(tx)?;
        let summary_slot =
            Self::maintenance_state_i64_in_transaction(tx, SUMMARY_ACTIVE_SLOT_STATE_KEY)?
                .filter(|slot| stats_trusted && matches!(slot, 0 | 1));
        let projection_slot =
            Self::sqlite_vec0_mutation_slot_in_transaction(tx, canonical_generation)?;
        Ok((
            canonical_generation,
            stats_trusted,
            summary_slot,
            projection_slot,
        ))
    }

    fn sqlite_vec0_mutation_slot_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        canonical_generation: i64,
    ) -> Result<Option<i64>> {
        let active_slot =
            Self::maintenance_state_i64_in_transaction(tx, SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
                .filter(|slot| matches!(slot, 0 | 1));
        if canonical_generation <= 0
            || Self::maintenance_state_i64_in_transaction(tx, SQLITE_VEC0_READY_STATE_KEY)?
                != Some(SEMANTIC_SQLITE_VEC0_PROJECTION_VERSION)
            || Self::maintenance_state_i64_in_transaction(tx, SQLITE_VEC0_GENERATION_STATE_KEY)?
                != Some(canonical_generation)
            || active_slot.is_none()
            || !register_sqlite_vec_auto_extension()
            || tx
                .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
                .is_err()
            || !sqlite_table_exists(tx, SQLITE_VEC0_TABLE)?
            || !sqlite_table_exists(tx, SQLITE_VEC0_META_TABLE)?
            || !Self::sqlite_vec0_schema_compatible_on_connection(tx)?
        {
            return Ok(None);
        }
        Ok(active_slot)
    }

    fn sidecar_trust_state(&self) -> Result<SemanticSidecarTrustState> {
        Ok(if self.stats_and_summary_trusted()? {
            SemanticSidecarTrustState::Ready
        } else {
            SemanticSidecarTrustState::Pending
        })
    }

    fn maintenance_state_i64(&self, key: &str) -> Result<Option<i64>> {
        Self::maintenance_state_i64_on_connection(&self.conn, key)
    }

    fn maintenance_state_i64_on_connection(conn: &Connection, key: &str) -> Result<Option<i64>> {
        if !sqlite_table_exists(conn, "semantic_maintenance_state")? {
            return Ok(None);
        }
        let key = Self::maintenance_state_key(key);
        let value = conn
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

    fn maintenance_state_string(&self, key: &str) -> Result<Option<String>> {
        if !sqlite_table_exists(&self.conn, "semantic_maintenance_state")? {
            return Ok(None);
        }
        let key = Self::maintenance_state_key(key);
        self.conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn set_maintenance_state_string(&self, key: &str, value: &str) -> Result<()> {
        let key = Self::maintenance_state_key(key);
        self.conn.execute(
            r#"
            INSERT INTO semantic_maintenance_state (key, value, updated_at_ms)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![key, value, utc_now().timestamp_millis()],
        )?;
        Ok(())
    }

    fn set_maintenance_state_i64_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        key: &str,
        value: i64,
    ) -> Result<()> {
        let key = Self::maintenance_state_key(key);
        tx.execute(
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

    fn maintenance_state_i64_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<Option<i64>> {
        Self::maintenance_state_i64_on_connection(tx, key)
    }

    fn increment_maintenance_state_i64_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        key: &str,
        delta: i64,
    ) -> Result<()> {
        let value = Self::maintenance_state_i64_in_transaction(tx, key)?
            .unwrap_or(0)
            .checked_add(delta)
            .ok_or_else(|| anyhow!("semantic maintenance counter overflow"))?;
        Self::set_maintenance_state_i64_in_transaction(tx, key, value)
    }

    fn global_maintenance_state_i64(&self, key: &str) -> Result<Option<i64>> {
        Self::global_maintenance_state_i64_on_connection(&self.conn, key)
    }

    fn global_maintenance_state_i64_on_connection(
        conn: &Connection,
        key: &str,
    ) -> Result<Option<i64>> {
        if !sqlite_table_exists(conn, "semantic_maintenance_state")? {
            return Ok(None);
        }
        let value = conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.and_then(|value| value.parse::<i64>().ok()))
    }

    fn global_maintenance_state_string(&self, key: &str) -> Result<Option<String>> {
        if !sqlite_table_exists(&self.conn, "semantic_maintenance_state")? {
            return Ok(None);
        }
        self.conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn set_global_maintenance_state_i64_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        key: &str,
        value: i64,
    ) -> Result<()> {
        tx.execute(
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

    fn set_global_maintenance_state_string_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        key: &str,
        value: &str,
    ) -> Result<()> {
        tx.execute(
            r#"
            INSERT INTO semantic_maintenance_state (key, value, updated_at_ms)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![key, value, utc_now().timestamp_millis()],
        )?;
        Ok(())
    }

    fn delete_maintenance_state_keys(&self, keys: &[&str]) -> Result<()> {
        if keys.is_empty() || !sqlite_table_exists(&self.conn, "semantic_maintenance_state")? {
            return Ok(());
        }
        for key in keys {
            let key = Self::maintenance_state_key(key);
            self.conn.execute(
                "DELETE FROM semantic_maintenance_state WHERE key = ?1",
                [key],
            )?;
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

    fn bounded_dirty_event_count(&self) -> Result<usize> {
        if !sqlite_table_exists(&self.conn, "semantic_dirty_events")? {
            return Ok(0);
        }
        let limit = SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT.saturating_add(1);
        let count = self.conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM (
                SELECT 1
                FROM semantic_dirty_events
                    INDEXED BY idx_semantic_dirty_events_model_priority
                WHERE model_key = ?1
                LIMIT ?2
            )
            "#,
            params![semantic_model_key(), limit as i64],
            |row| row.get::<_, i64>(0),
        )?;
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
                    semantic_model_key(),
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
        let bounded_limit = limit.min(SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT);
        let mut present_stmt = self.conn.prepare(
            r#"
            SELECT event_id, priority_seq, queued_at_ms
            FROM semantic_dirty_events
            WHERE model_key = ?1 AND priority_seq IS NOT NULL
            ORDER BY priority_seq DESC, queued_at_ms DESC
            LIMIT ?2
            "#,
        )?;
        let present =
            present_stmt.query_map(params![semantic_model_key(), bounded_limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
        let mut present = present.collect::<rusqlite::Result<Vec<_>>>()?;
        present.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.0.cmp(&right.0))
        });
        let mut event_ids = present
            .into_iter()
            .map(|(event_id, _, _)| {
                Uuid::parse_str(&event_id)
                    .context("invalid dirty event id in semantic vector store")
            })
            .collect::<Result<Vec<_>>>()?;
        let remaining = bounded_limit.saturating_sub(event_ids.len());
        if remaining > 0 {
            let mut absent_stmt = self.conn.prepare(
                r#"
                SELECT event_id
                FROM semantic_dirty_events
                WHERE model_key = ?1 AND priority_seq IS NULL
                ORDER BY queued_at_ms ASC
                LIMIT ?2
                "#,
            )?;
            let absent = absent_stmt
                .query_map(params![semantic_model_key(), remaining as i64], |row| {
                    row.get::<_, String>(0)
                })?;
            for event_id in absent {
                event_ids.push(
                    Uuid::parse_str(&event_id?)
                        .context("invalid dirty event id in semantic vector store")?,
                );
            }
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
                    stmt.execute(params![semantic_model_key(), event_id.to_string()])?,
                );
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    fn plaintext_value_count(&self) -> Result<usize> {
        Ok(
            (self.global_maintenance_state_i64(PLAINTEXT_SANITIZED_GLOBAL_STATE_KEY)?
                != Some(PLAINTEXT_SANITIZED_STATE_VERSION)) as usize,
        )
    }

    fn existing_hashes_for_event_ids(&self, event_ids: &[Uuid]) -> Result<HashMap<Uuid, String>> {
        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        if event_ids.len() > SEMANTIC_DIRTY_QUEUE_RECENT_LIMIT {
            return Err(SemanticVectorStorePending::new(
                "semantic hash lookup exceeds the bounded event-id limit",
            )
            .into());
        }
        if !self.stats_and_summary_trusted()? {
            return Err(SemanticVectorStorePending::new(
                "semantic summary is not trusted for hash lookup",
            )
            .into());
        }
        let slot = self
            .maintenance_state_i64(SUMMARY_ACTIVE_SLOT_STATE_KEY)?
            .filter(|slot| matches!(slot, 0 | 1))
            .ok_or_else(|| SemanticVectorStorePending::new("semantic summary slot is missing"))?;
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT event_id, source_text_sha256, single_source_hash
            FROM semantic_event_summary
            WHERE slot = ? AND model_key = ?
              AND event_id IN ({placeholders})
            "#
        );
        let mut query_params = vec![
            SqlValue::from(slot),
            SqlValue::from(semantic_model_key().to_owned()),
        ];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut hashes = HashMap::new();
        while let Some(row) = rows.next()? {
            if row.get::<_, i64>(2)? != 1 {
                continue;
            }
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            hashes.insert(event_id, row.get(1)?);
        }
        Ok(hashes)
    }

    #[cfg(test)]
    fn upsert_chunk_embeddings(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let nominal_bytes = items.iter().fold(0_u64, |total, (_, embedding)| {
            total.saturating_add(
                (embedding.len() as u64)
                    .saturating_mul(std::mem::size_of::<f32>() as u64)
                    .saturating_mul(2),
            )
        });
        ctx_history_capture::pace_current_disk_io(nominal_bytes);
        let supplemental_bytes = self.upsert_chunk_embeddings_precharged(items, nominal_bytes)?;
        ctx_history_capture::pace_current_disk_io(supplemental_bytes);
        Ok(())
    }

    fn upsert_chunk_embeddings_precharged(
        &mut self,
        items: &[(SemanticChunkDocument, Vec<f32>)],
        nominal_bytes: u64,
    ) -> Result<u64> {
        if items.is_empty() {
            return Ok(0);
        }
        let pacing = self.begin_write_pacing(nominal_bytes);
        let mut event_summaries = HashMap::<Uuid, (u64, String, usize)>::new();
        for (doc, _) in items {
            event_summaries
                .entry(doc.event_id)
                .and_modify(|(_, _, chunk_count)| {
                    *chunk_count = chunk_count.saturating_add(1);
                })
                .or_insert_with(|| (doc.seq, doc.source_text_hash.clone(), 1));
        }
        let event_ids = event_summaries.keys().copied().collect::<HashSet<_>>();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (canonical_generation, stats_trusted, summary_slot, projection_slot) =
            Self::canonical_mutation_state_in_transaction(&tx)?;
        let next_generation = canonical_generation
            .checked_add(1)
            .filter(|generation| *generation > 0)
            .ok_or_else(|| anyhow!("semantic canonical generation overflow"))?;
        let projection_trusted = projection_slot.is_some();
        let mut deleted_items = 0_usize;
        let mut deleted_chunks = 0_usize;
        let mut deleted_projection_vectors = 0_usize;
        let mut deleted_projection_meta = 0_usize;
        let mut inserted_events = HashSet::new();
        let mut inserted_chunks = 0_usize;
        {
            if let Some(slot) = projection_slot.filter(|_| projection_trusted) {
                let mut select_stmt = tx.prepare(&format!(
                    r#"
                    SELECT rowid FROM {SQLITE_VEC0_META_TABLE}
                    WHERE slot = ?1 AND model_key = ?2 AND event_id = ?3
                    "#
                ))?;
                let mut delete_vec_stmt =
                    tx.prepare(&format!("DELETE FROM {SQLITE_VEC0_TABLE} WHERE rowid = ?1"))?;
                let mut delete_meta_stmt = tx.prepare(&format!(
                    "DELETE FROM {SQLITE_VEC0_META_TABLE} WHERE rowid = ?1"
                ))?;
                for event_id in &event_ids {
                    let event_id = event_id.to_string();
                    let rowids = {
                        let rows = select_stmt
                            .query_map(params![slot, semantic_model_key(), &event_id], |row| {
                                row.get::<_, i64>(0)
                            })?;
                        rows.collect::<rusqlite::Result<Vec<_>>>()?
                    };
                    for rowid in rowids {
                        deleted_projection_vectors = deleted_projection_vectors
                            .saturating_add(delete_vec_stmt.execute([rowid])?);
                        deleted_projection_meta = deleted_projection_meta
                            .saturating_add(delete_meta_stmt.execute([rowid])?);
                    }
                }
            }
            let mut delete_stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE event_id = ?1 AND model_key = ?2",
            )?;
            for event_id in &event_ids {
                let rows =
                    delete_stmt.execute(params![event_id.to_string(), semantic_model_key()])?;
                if rows > 0 {
                    deleted_items = deleted_items.saturating_add(1);
                    deleted_chunks = deleted_chunks.saturating_add(rows);
                }
            }
            drop(delete_stmt);

            if let Some(summary_slot) = summary_slot {
                let mut stmt = tx.prepare(
                    "DELETE FROM semantic_event_summary WHERE slot = ?1 AND model_key = ?2 AND event_id = ?3",
                )?;
                for event_id in &event_ids {
                    stmt.execute(params![
                        summary_slot,
                        semantic_model_key(),
                        event_id.to_string()
                    ])?;
                }
            }

            let mut stmt = tx.prepare(
                r#"
                INSERT INTO event_embedding_chunks
                    (event_id, model_key, history_record_id, session_id, event_seq,
                     chunk_index, chunk_count, source_text_sha256, chunk_text_sha256,
                     chunk_text, start_char, end_char, dimensions, embedding_f32, embedded_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
            )?;
            let mut vec0_meta_stmt = if projection_trusted {
                Some(tx.prepare(&format!(
                    r#"
                        INSERT INTO {SQLITE_VEC0_META_TABLE}
                            (slot, canonical_rowid, event_id, model_key, history_record_id,
                             session_id, event_seq, chunk_index, source_text_sha256,
                             start_char, end_char)
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                        "#
                ))?)
            } else {
                None
            };
            let mut vec0_stmt = if projection_trusted {
                Some(tx.prepare(&format!(
                    "INSERT INTO {SQLITE_VEC0_TABLE}(rowid, embedding, embedding_coarse, slot, model_key) VALUES (?1, ?2, vec_quantize_binary(?2), ?3, ?4)"
                ))?)
            } else {
                None
            };
            let embedded_at_ms = utc_now().timestamp_millis();
            for (doc, embedding) in items {
                let event_id = doc.event_id.to_string();
                let history_record_id = doc.history_record_id.map(|id| id.to_string());
                let session_id = doc.session_id.map(|id| id.to_string());
                let blob = serialize_f32_blob(embedding);
                let inserted = stmt.execute(params![
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
                inserted_chunks = inserted_chunks.saturating_add(inserted);
                if inserted > 0 {
                    inserted_events.insert(doc.event_id);
                }
                let canonical_rowid = tx.last_insert_rowid();
                if let (Some(meta_stmt), Some(vec_stmt)) =
                    (vec0_meta_stmt.as_mut(), vec0_stmt.as_mut())
                {
                    let slot = projection_slot.ok_or_else(|| {
                        anyhow!("trusted semantic projection is missing its active slot")
                    })?;
                    meta_stmt.execute(params![
                        slot,
                        canonical_rowid,
                        &event_id,
                        semantic_model_key(),
                        &history_record_id,
                        &session_id,
                        doc.seq as i64,
                        doc.chunk_index as i64,
                        &doc.source_text_hash,
                        doc.start_char as i64,
                        doc.end_char as i64,
                    ])?;
                    let projection_rowid = tx.last_insert_rowid();
                    vec_stmt.execute(params![
                        projection_rowid,
                        &blob,
                        slot,
                        semantic_model_key()
                    ])?;
                }
            }
            drop(stmt);
            drop(vec0_meta_stmt);
            drop(vec0_stmt);

            if let Some(slot) = summary_slot {
                let mut summary_stmt = tx.prepare(
                    r#"
                    INSERT INTO semantic_event_summary
                        (slot, model_key, event_id, event_seq, source_text_sha256,
                         single_source_hash, chunk_count)
                    VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
                    "#,
                )?;
                for event_id in &inserted_events {
                    let (event_seq, source_text_hash, chunk_count) = event_summaries
                        .get(event_id)
                        .ok_or_else(|| anyhow!("inserted semantic event has no document"))?;
                    summary_stmt.execute(params![
                        slot,
                        semantic_model_key(),
                        event_id.to_string(),
                        *event_seq as i64,
                        source_text_hash,
                        *chunk_count as i64
                    ])?;
                }
            }

            let cached = tx
                .query_row(
                    r#"
                    SELECT embedded_items, embedded_chunks
                    FROM semantic_index_stats
                    WHERE model_key = ?1 AND trust_version = ?2 AND generation = ?3
                    "#,
                    params![
                        semantic_model_key(),
                        SEMANTIC_SIDECAR_TRUST_VERSION,
                        canonical_generation
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let counts_valid = stats_trusted
                && (!projection_trusted
                    || (deleted_projection_vectors == deleted_chunks
                        && deleted_projection_meta == deleted_chunks))
                && cached.is_some_and(|(items, chunks)| {
                    items >= deleted_items as i64 && chunks >= deleted_chunks as i64
                });
            if counts_valid {
                let (items_before, chunks_before) = cached.unwrap_or_default();
                tx.execute(
                    r#"
                    UPDATE semantic_index_stats
                    SET embedded_items = ?2,
                        embedded_chunks = ?3,
                        updated_at_ms = ?4,
                        generation = ?5
                    WHERE model_key = ?1
                    "#,
                    params![
                        semantic_model_key(),
                        items_before - deleted_items as i64 + inserted_events.len() as i64,
                        chunks_before - deleted_chunks as i64 + inserted_chunks as i64,
                        utc_now().timestamp_millis(),
                        next_generation
                    ],
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SUMMARY_GENERATION_STATE_KEY,
                    next_generation,
                )?;
            } else {
                tx.execute(
                    "UPDATE semantic_index_stats SET trust_version = 0 WHERE model_key = ?1",
                    [semantic_model_key()],
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SUMMARY_GENERATION_STATE_KEY,
                    0,
                )?;
            }
            if projection_trusted && counts_valid {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_GENERATION_STATE_KEY,
                    next_generation,
                )?;
            } else {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_READY_STATE_KEY,
                    0,
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_GENERATION_STATE_KEY,
                    0,
                )?;
            }
            Self::set_maintenance_state_i64_in_transaction(
                &tx,
                CANONICAL_GENERATION_STATE_KEY,
                next_generation,
            )?;
        }
        tx.commit()?;
        Ok(self.finish_write_pacing(pacing))
    }

    #[cfg(test)]
    fn prune_ineligible_events(&mut self, store: &Store) -> Result<SemanticPruneOutcome> {
        ctx_history_capture::pace_current_disk_io(SEMANTIC_SIDECAR_MAINTENANCE_LOGICAL_BYTES);
        self.prune_ineligible_events_precharged(store)
    }

    fn prune_ineligible_events_precharged(
        &mut self,
        store: &Store,
    ) -> Result<SemanticPruneOutcome> {
        let sql_deadline =
            Instant::now() + StdDuration::from_millis(SEMANTIC_SIDECAR_MAINTENANCE_MAX_MILLIS);
        self.conn.progress_handler(
            SEMANTIC_SQL_PROGRESS_OPS,
            Some(move || Instant::now() >= sql_deadline),
        );
        let page_units = match self.maintenance_page_units() {
            Ok(units) => units,
            Err(error) => {
                self.conn.progress_handler(0, None::<fn() -> bool>);
                return Err(error);
            }
        };
        let result = self.prune_ineligible_events_precharged_inner(store, page_units);
        self.conn.progress_handler(0, None::<fn() -> bool>);
        match result {
            Err(error) if Self::sqlite_operation_interrupted(&error) => {
                if page_units == 1 {
                    Err(SemanticVectorStoreTerminal::new(
                        "semantic prune made no progress at the one-row floor",
                    )
                    .into())
                } else {
                    self.set_maintenance_state_i64(
                        MAINTENANCE_PAGE_UNITS_STATE_KEY,
                        (page_units / 2).max(1) as i64,
                    )?;
                    Ok(SemanticPruneOutcome::default())
                }
            }
            Ok(outcome) => {
                self.grow_maintenance_page_units_after_success(page_units)?;
                Ok(outcome)
            }
            Err(error) => Err(error),
        }
    }

    fn prune_ineligible_events_precharged_inner(
        &mut self,
        store: &Store,
        page_units: usize,
    ) -> Result<SemanticPruneOutcome> {
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticPruneOutcome::default());
        }
        let cursor = self
            .maintenance_state_i64("prune_event_seq_before")?
            .zip(self.maintenance_state_string("prune_event_id_before")?);
        let (mut canonical_snapshot, mut sidecar_events) =
            self.prune_candidate_events(cursor.as_ref(), page_units)?;
        if sidecar_events.is_empty() && cursor.is_some() {
            (canonical_snapshot, sidecar_events) = self.prune_candidate_events(None, page_units)?;
        }
        let mut admitted_chunks = 0_usize;
        let mut admitted_events = Vec::new();
        for event in sidecar_events {
            let declared_chunks = event.4;
            if declared_chunks > page_units {
                return Err(SemanticVectorStoreTerminal::new(format!(
                    "semantic prune event {} declares {declared_chunks} chunks above the slice limit",
                    event.0
                ))
                .into());
            }
            if !admitted_events.is_empty()
                && admitted_chunks.saturating_add(declared_chunks) > page_units
            {
                break;
            }
            admitted_chunks = admitted_chunks.saturating_add(declared_chunks);
            admitted_events.push(event);
        }
        let next_cursor = admitted_events
            .last()
            .map(|(event_id, _, _, event_seq, _)| (*event_seq, event_id.to_string()));
        let mut outcome = SemanticPruneOutcome::default();
        if !admitted_events.is_empty() {
            let event_ids = admitted_events
                .iter()
                .map(|(event_id, _, _, _, _)| *event_id)
                .collect::<Vec<_>>();
            let eligible_event_ids = store.semantic_eligible_event_ids(&event_ids)?;
            let current_docs = store.event_embedding_documents_by_ids(&event_ids)?;
            let current_by_id = current_docs
                .into_iter()
                .map(|doc| (doc.event_id, doc))
                .collect::<HashMap<_, _>>();
            let mut delete_event_ids = Vec::new();
            let mut stale_docs = Vec::new();
            for (event_id, stored_hash, single_hash, _, _) in &admitted_events {
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
            if !stale_docs.is_empty() {
                outcome.queued_stale_events = outcome
                    .queued_stale_events
                    .saturating_add(self.enqueue_dirty_documents(&stale_docs, "stale_hash")?);
            }
            let Some(canonical_snapshot) = canonical_snapshot else {
                return Ok(outcome);
            };
            let (deleted_chunks, supplemental_bytes, snapshot_unchanged) = self
                .delete_embedding_chunks_for_event_ids_precharged_inner(
                    &delete_event_ids,
                    Some(canonical_snapshot),
                )?;
            if !snapshot_unchanged {
                outcome.supplemental_bytes = supplemental_bytes;
                return Ok(outcome);
            }
            if deleted_chunks > admitted_chunks {
                return Err(SemanticVectorStoreTerminal::new(
                    "semantic prune deleted more canonical rows than the admitted summary slice",
                )
                .into());
            }
            outcome.deleted_chunks = deleted_chunks;
            outcome.supplemental_bytes = supplemental_bytes;
        }
        if let Some((event_seq, event_id)) = next_cursor {
            self.set_maintenance_state_i64("prune_event_seq_before", event_seq)?;
            self.set_maintenance_state_string("prune_event_id_before", &event_id)?;
        }
        Ok(outcome)
    }

    fn prune_candidate_events(
        &mut self,
        cursor: Option<&(i64, String)>,
        page_units: usize,
    ) -> Result<(Option<(i64, i64)>, Vec<(Uuid, String, bool, i64, usize)>)> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (canonical_generation, stats_trusted, summary_slot, _) =
            Self::canonical_mutation_state_in_transaction(&tx)?;
        let Some(slot) = summary_slot.filter(|_| stats_trusted) else {
            tx.commit()?;
            return Ok((None, Vec::new()));
        };
        let (sql, query_params): (&str, Vec<SqlValue>) = match cursor {
            Some((event_seq, event_id)) => (
                r#"
                SELECT event_id, source_text_sha256, single_source_hash, event_seq, chunk_count
                FROM semantic_event_summary
                WHERE slot = ?1 AND model_key = ?2
                  AND (event_seq, event_id) < (?3, ?4)
                ORDER BY event_seq DESC, event_id DESC
                LIMIT ?5
                "#,
                vec![
                    SqlValue::from(slot),
                    SqlValue::from(semantic_model_key().to_owned()),
                    SqlValue::from(*event_seq),
                    SqlValue::from(event_id.clone()),
                    SqlValue::from(page_units as i64),
                ],
            ),
            None => (
                r#"
                SELECT event_id, source_text_sha256, single_source_hash, event_seq, chunk_count
                FROM semantic_event_summary
                WHERE slot = ?1 AND model_key = ?2
                ORDER BY event_seq DESC, event_id DESC
                LIMIT ?3
                "#,
                vec![
                    SqlValue::from(slot),
                    SqlValue::from(semantic_model_key().to_owned()),
                    SqlValue::from(page_units as i64),
                ],
            ),
        };
        let mut stmt = tx.prepare(sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut sidecar_events = Vec::new();
        while let Some(row) = rows.next()? {
            let event_id_text = row.get::<_, String>(0)?;
            if let Ok(event_id) = Uuid::parse_str(&event_id_text) {
                let source_text_hash = row.get::<_, String>(1)?;
                let single_hash = row.get::<_, i64>(2)? == 1;
                let event_seq = row.get::<_, i64>(3)?.max(0);
                let chunk_count = row.get::<_, i64>(4)?.max(0) as usize;
                sidecar_events.push((
                    event_id,
                    source_text_hash,
                    single_hash,
                    event_seq,
                    chunk_count,
                ));
            }
        }
        drop(rows);
        drop(stmt);
        tx.commit()?;
        Ok((Some((canonical_generation, slot)), sidecar_events))
    }

    #[cfg(test)]
    fn delete_embedding_chunks_for_event_ids(&mut self, event_ids: &[Uuid]) -> Result<usize> {
        ctx_history_capture::pace_current_disk_io(SEMANTIC_SIDECAR_MAINTENANCE_LOGICAL_BYTES);
        let (deleted_chunks, supplemental_bytes) =
            self.delete_embedding_chunks_for_event_ids_precharged(event_ids)?;
        ctx_history_capture::pace_current_disk_io(supplemental_bytes);
        Ok(deleted_chunks)
    }

    #[cfg(test)]
    fn delete_embedding_chunks_for_event_ids_precharged(
        &mut self,
        event_ids: &[Uuid],
    ) -> Result<(usize, u64)> {
        let (deleted_chunks, supplemental_bytes, _) =
            self.delete_embedding_chunks_for_event_ids_precharged_inner(event_ids, None)?;
        Ok((deleted_chunks, supplemental_bytes))
    }

    fn delete_embedding_chunks_for_event_ids_precharged_inner(
        &mut self,
        event_ids: &[Uuid],
        expected_canonical_snapshot: Option<(i64, i64)>,
    ) -> Result<(usize, u64, bool)> {
        if event_ids.is_empty() || !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok((0, 0, true));
        }
        let page_units = self.maintenance_page_units()?;
        if event_ids.len() > page_units {
            return Err(SemanticVectorStoreTerminal::new(
                "semantic delete request exceeds the sidecar row slice",
            )
            .into());
        }
        let pacing = self.begin_write_pacing(SEMANTIC_SIDECAR_MAINTENANCE_LOGICAL_BYTES);
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT rowid, event_id, length(embedding_f32)
            FROM event_embedding_chunks
            WHERE model_key = ? AND event_id IN ({placeholders})
            ORDER BY rowid
            LIMIT ?
            "#
        );
        let mut query_params = vec![SqlValue::from(semantic_model_key().to_owned())];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        query_params.push(SqlValue::from(page_units.saturating_add(1) as i64));
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (canonical_generation, stats_trusted, summary_slot, projection_slot) =
            Self::canonical_mutation_state_in_transaction(&tx)?;
        let snapshot_unchanged = expected_canonical_snapshot.map_or(true, |expected| {
            summary_slot == Some(expected.1) && canonical_generation == expected.0
        });
        if !snapshot_unchanged {
            tx.commit()?;
            return Ok((0, self.finish_write_pacing(pacing), false));
        }
        let canonical_rows = {
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(query_params), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if canonical_rows.len() > page_units {
            return Err(SemanticVectorStoreTerminal::new(
                "semantic delete event exceeds the sidecar row slice",
            )
            .into());
        }
        let logical_bytes = canonical_rows
            .iter()
            .map(|(_, _, bytes)| *bytes)
            .sum::<u64>();
        if logical_bytes > SEMANTIC_SIDECAR_MAINTENANCE_MAX_BYTES as u64 {
            return Err(SemanticVectorStoreTerminal::new(
                "semantic delete vectors exceed the sidecar byte slice",
            )
            .into());
        }
        if canonical_rows.is_empty() {
            tx.commit()?;
            return Ok((0, self.finish_write_pacing(pacing), true));
        }
        let projection_trusted = projection_slot.is_some();
        let mut deleted_event_ids = HashSet::new();
        let mut deleted_chunks = 0_usize;
        let mut deleted_projection_vectors = 0_usize;
        let mut deleted_projection_meta = 0_usize;
        {
            if let Some(slot) = projection_slot.filter(|_| projection_trusted) {
                let mut select_stmt = tx.prepare(&format!(
                    r#"
                    SELECT rowid FROM {SQLITE_VEC0_META_TABLE}
                    WHERE slot = ?1 AND model_key = ?2 AND canonical_rowid = ?3
                    "#
                ))?;
                let mut delete_vec_stmt =
                    tx.prepare(&format!("DELETE FROM {SQLITE_VEC0_TABLE} WHERE rowid = ?1"))?;
                let mut delete_meta_stmt = tx.prepare(&format!(
                    "DELETE FROM {SQLITE_VEC0_META_TABLE} WHERE rowid = ?1"
                ))?;
                for (canonical_rowid, _, _) in &canonical_rows {
                    let projection_rowid = select_stmt
                        .query_row(
                            params![slot, semantic_model_key(), canonical_rowid],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?;
                    if let Some(rowid) = projection_rowid {
                        deleted_projection_vectors = deleted_projection_vectors
                            .saturating_add(delete_vec_stmt.execute([rowid])?);
                        deleted_projection_meta = deleted_projection_meta
                            .saturating_add(delete_meta_stmt.execute([rowid])?);
                    }
                }
            }
            let mut stmt = tx.prepare(
                "DELETE FROM event_embedding_chunks WHERE rowid = ?1 AND model_key = ?2",
            )?;
            for (rowid, event_id, _) in &canonical_rows {
                let rows = stmt.execute(params![rowid, semantic_model_key()])?;
                if rows == 1 {
                    deleted_event_ids.insert(event_id.clone());
                    deleted_chunks = deleted_chunks.saturating_add(1);
                }
            }
            drop(stmt);
            let deleted_items = deleted_event_ids.len();
            if deleted_chunks > canonical_rows.len()
                || deleted_projection_vectors > canonical_rows.len()
                || deleted_projection_meta > canonical_rows.len()
            {
                return Err(SemanticVectorStoreTerminal::new(
                    "semantic delete exceeded its admitted canonical row slice",
                )
                .into());
            }
            if deleted_chunks == 0
                && deleted_projection_vectors == 0
                && deleted_projection_meta == 0
            {
                tx.commit()?;
                return Ok((0, self.finish_write_pacing(pacing), true));
            }
            if let Some(slot) = summary_slot {
                let mut stmt = tx.prepare(
                    "DELETE FROM semantic_event_summary WHERE slot = ?1 AND model_key = ?2 AND event_id = ?3",
                )?;
                for event_id in event_ids {
                    stmt.execute(params![slot, semantic_model_key(), event_id.to_string()])?;
                }
            }
            let next_generation = canonical_generation
                .checked_add(1)
                .filter(|generation| *generation > 0)
                .ok_or_else(|| anyhow!("semantic canonical generation overflow"))?;
            let cached = tx
                .query_row(
                    r#"
                    SELECT embedded_items, embedded_chunks
                    FROM semantic_index_stats
                    WHERE model_key = ?1 AND trust_version = ?2 AND generation = ?3
                    "#,
                    params![
                        semantic_model_key(),
                        SEMANTIC_SIDECAR_TRUST_VERSION,
                        canonical_generation
                    ],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let counts_valid = stats_trusted
                && (!projection_trusted
                    || (deleted_projection_vectors == deleted_chunks
                        && deleted_projection_meta == deleted_chunks))
                && cached.is_some_and(|(items, chunks)| {
                    items >= deleted_items as i64 && chunks >= deleted_chunks as i64
                });
            if counts_valid {
                let (items_before, chunks_before) = cached.unwrap_or_default();
                tx.execute(
                    r#"
                    UPDATE semantic_index_stats
                    SET embedded_items = ?2,
                        embedded_chunks = ?3,
                        updated_at_ms = ?4,
                        generation = ?5
                    WHERE model_key = ?1
                    "#,
                    params![
                        semantic_model_key(),
                        items_before - deleted_items as i64,
                        chunks_before - deleted_chunks as i64,
                        utc_now().timestamp_millis(),
                        next_generation
                    ],
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SUMMARY_GENERATION_STATE_KEY,
                    next_generation,
                )?;
            } else {
                tx.execute(
                    "UPDATE semantic_index_stats SET trust_version = 0 WHERE model_key = ?1",
                    [semantic_model_key()],
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SUMMARY_GENERATION_STATE_KEY,
                    0,
                )?;
            }
            if projection_trusted && counts_valid {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_GENERATION_STATE_KEY,
                    next_generation,
                )?;
            } else {
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_READY_STATE_KEY,
                    0,
                )?;
                Self::set_maintenance_state_i64_in_transaction(
                    &tx,
                    SQLITE_VEC0_GENERATION_STATE_KEY,
                    0,
                )?;
            }
            Self::set_maintenance_state_i64_in_transaction(
                &tx,
                CANONICAL_GENERATION_STATE_KEY,
                next_generation,
            )?;
        }
        tx.commit()?;
        Ok((deleted_chunks, self.finish_write_pacing(pacing), true))
    }
}
