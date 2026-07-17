fn semantic_exact_vector_bytes() -> usize {
    SEMANTIC_DIMENSIONS.saturating_mul(std::mem::size_of::<f32>())
}

fn semantic_sqlite_vec0_candidate_limit(
    limit: usize,
    candidate_row_limit: usize,
    embedded_chunks: usize,
) -> usize {
    limit
        .max(1)
        .saturating_mul(SEMANTIC_SQLITE_VEC0_OVERFETCH_FACTOR)
        .max(SEMANTIC_SQLITE_VEC0_MIN_CANDIDATES)
        .min(SEMANTIC_SQLITE_VEC0_MAX_K)
        .min(candidate_row_limit.max(1))
        .min(embedded_chunks.max(1))
}

fn semantic_sqlite_vec0_scan_bytes(
    embedded_chunks: usize,
    exact_candidates: usize,
) -> Option<usize> {
    embedded_chunks
        .checked_mul(SEMANTIC_BINARY_VECTOR_BYTES)?
        .checked_add(exact_candidates.checked_mul(semantic_exact_vector_bytes())?)
}

fn semantic_sqlite_vec0_full_scan_ready(stats: SemanticSidecarStats) -> bool {
    semantic_sqlite_vec0_scan_bytes(
        stats.embedded_chunks,
        stats.embedded_chunks.min(SEMANTIC_SQLITE_VEC0_MAX_K),
    )
    .is_some_and(|bytes| bytes <= SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES)
}

fn retain_best_semantic_chunk(
    best_by_event: &mut HashMap<Uuid, SemanticVectorHit>,
    query_embedding: &[f32],
    event_id: Uuid,
    source_text_hash: String,
    start_char: usize,
    end_char: usize,
    embedding: &[u8],
) -> Result<()> {
    let Some(similarity) = dot_product_f32_blob(query_embedding, embedding)? else {
        return Ok(());
    };
    let candidate = SemanticVectorHit {
        event_id,
        similarity,
        source_text_hash,
        start_char,
        end_char,
    };
    match best_by_event.get_mut(&event_id) {
        Some(existing) if similarity > existing.similarity => *existing = candidate,
        None => {
            best_by_event.insert(event_id, candidate);
        }
        _ => {}
    }
    Ok(())
}

fn finish_semantic_search(
    best_by_event: HashMap<Uuid, SemanticVectorHit>,
    limit: usize,
    scan_started: Instant,
    chunks_scanned: usize,
    vector_bytes_read: usize,
) -> SemanticVectorSearch {
    let events_scored = best_by_event.len();
    let mut hits = best_by_event.into_values().collect::<Vec<_>>();
    let limit = limit.max(1);
    if hits.len() > limit {
        hits.select_nth_unstable_by(limit - 1, compare_semantic_hits_desc);
        hits.truncate(limit);
    }
    hits.sort_by(compare_semantic_hits_desc);
    SemanticVectorSearch {
        hits,
        stats: SemanticVectorSearchStats {
            backend: Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC),
            scan_ms: scan_started.elapsed().as_millis() as u64,
            chunks_scanned,
            vector_bytes_read,
            events_scored,
        },
    }
}

impl SemanticVectorStore {
    #[cfg(test)]
    fn search(&self, query_embedding: &[f32], limit: usize) -> Result<SemanticVectorSearch> {
        self.search_until(
            query_embedding,
            limit,
            Instant::now() + SEMANTIC_VECTOR_SEARCH_TIMEOUT,
        )
    }

    #[cfg(test)]
    fn search_until(
        &self,
        query_embedding: &[f32],
        limit: usize,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        self.search_until_bounded(query_embedding, limit, SEMANTIC_SQLITE_VEC0_MAX_K, deadline)
    }

    fn search_until_bounded(
        &self,
        query_embedding: &[f32],
        limit: usize,
        candidate_row_limit: usize,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        self.search_with_event_filter(query_embedding, limit, candidate_row_limit, None, deadline)
    }

    #[cfg(test)]
    fn search_event_ids(
        &self,
        query_embedding: &[f32],
        event_ids: &[Uuid],
        limit: usize,
    ) -> Result<SemanticVectorSearch> {
        if event_ids.is_empty() {
            return Ok(SemanticVectorSearch::default());
        }
        self.search_event_ids_until(
            query_embedding,
            event_ids,
            limit,
            Instant::now() + SEMANTIC_VECTOR_SEARCH_TIMEOUT,
        )
    }

    fn search_event_ids_until(
        &self,
        query_embedding: &[f32],
        event_ids: &[Uuid],
        limit: usize,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        if event_ids.is_empty() {
            return Ok(SemanticVectorSearch::default());
        }
        self.search_with_event_filter(
            query_embedding,
            limit,
            event_ids.len(),
            Some(event_ids),
            deadline,
        )
    }

    fn search_with_event_filter(
        &self,
        query_embedding: &[f32],
        limit: usize,
        candidate_row_limit: usize,
        event_ids: Option<&[Uuid]>,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        if Instant::now() >= deadline {
            return Err(SemanticVectorStorePending::new(
                "semantic vector retrieval deadline elapsed",
            )
            .into());
        }
        self.conn.progress_handler(
            SEMANTIC_SQL_PROGRESS_OPS,
            Some(move || Instant::now() >= deadline),
        );
        let result = self.search_with_event_filter_inner(
            query_embedding,
            limit,
            candidate_row_limit,
            event_ids,
            deadline,
        );
        self.conn.progress_handler(0, None::<fn() -> bool>);
        let deadline_elapsed = Instant::now() >= deadline;
        match result {
            Err(_) if deadline_elapsed => Err(SemanticVectorStorePending::new(
                "semantic vector retrieval deadline elapsed",
            )
            .into()),
            Ok(_) if deadline_elapsed => Err(SemanticVectorStorePending::new(
                "semantic vector retrieval deadline elapsed",
            )
            .into()),
            result => result,
        }
    }

    fn search_with_event_filter_inner(
        &self,
        query_embedding: &[f32],
        limit: usize,
        candidate_row_limit: usize,
        event_ids: Option<&[Uuid]>,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        if query_embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(anyhow!(
                "semantic query embedding has {} dimensions; expected {SEMANTIC_DIMENSIONS}",
                query_embedding.len()
            ));
        }
        if event_ids.is_some_and(|ids| {
            ids.len() > query_service_contract::SEMANTIC_QUERY_MAX_CANDIDATE_EVENT_IDS
        }) {
            return Err(SemanticVectorStorePending::new(
                "semantic candidate event set exceeds the bounded SQL variable limit",
            )
            .into());
        }
        let stats = self
            .cached_stats()?
            .ok_or_else(|| SemanticVectorStorePending::new("cached stats are untrusted"))?;
        if !self.sqlite_vec0_search_ready()? {
            return Err(SemanticVectorStorePending::new(
                "trusted semantic vector projection is not ready",
            )
            .into());
        }
        match event_ids {
            Some(event_ids) => {
                self.search_sqlite_vec0_event_ids(query_embedding, event_ids, limit, deadline)
            }
            None => self.search_sqlite_vec0(
                query_embedding,
                limit,
                candidate_row_limit,
                stats,
                deadline,
            ),
        }
    }

    fn search_sqlite_vec0(
        &self,
        query_embedding: &[f32],
        limit: usize,
        candidate_row_limit: usize,
        stats: SemanticSidecarStats,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        let scan_started = Instant::now();
        let exact_candidate_limit =
            semantic_sqlite_vec0_candidate_limit(limit, candidate_row_limit, stats.embedded_chunks);
        let Some(maximum_vector_bytes) =
            semantic_sqlite_vec0_scan_bytes(stats.embedded_chunks, exact_candidate_limit)
        else {
            return Err(SemanticVectorStorePending::new(
                "semantic binary retrieval byte budget overflowed",
            )
            .into());
        };
        if maximum_vector_bytes > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES {
            return Err(SemanticVectorStorePending::new(
                "trusted corpus exceeds the bounded sqlite vec0 binary scan",
            )
            .into());
        }
        let query_blob = serialize_f32_blob(query_embedding);
        let slot = self
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .ok_or_else(|| SemanticVectorStorePending::new("projection slot is missing"))?;
        if Instant::now() >= deadline {
            return Err(SemanticVectorStorePending::new(
                "semantic vector retrieval deadline elapsed before sqlite vec0 scan",
            )
            .into());
        }
        let sql = format!(
            r#"
            WITH coarse_matches AS (
                SELECT rowid, embedding
                FROM {SQLITE_VEC0_TABLE}
                WHERE slot = ?1
                  AND model_key = ?2
                  AND embedding_coarse MATCH vec_quantize_binary(?3)
                  AND k = ?4
                ORDER BY distance
            )
            SELECT CASE WHEN length(m.event_id) = 36 THEN m.event_id END,
                   CASE WHEN length(m.source_text_sha256) = 64
                        THEN m.source_text_sha256 END,
                   m.start_char, m.end_char,
                   c.embedding
            FROM coarse_matches AS c
            JOIN {SQLITE_VEC0_META_TABLE} AS m ON m.rowid = c.rowid
            WHERE m.slot = ?1 AND m.model_key = ?2
            "#
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![
            slot,
            semantic_model_key(),
            &query_blob,
            exact_candidate_limit as i64
        ])?;
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let mut exact_rows = 0_usize;
        let mut exact_vector_bytes = 0_usize;
        while let Some(row) = rows.next()? {
            if Instant::now() >= deadline {
                return Err(SemanticVectorStorePending::new(
                    "semantic vector retrieval deadline elapsed during exact rerank",
                )
                .into());
            }
            exact_rows = exact_rows.saturating_add(1);
            if exact_rows > exact_candidate_limit {
                return Err(SemanticVectorStorePending::new(
                    "sqlite vec0 returned candidates beyond its exact rerank bound",
                )
                .into());
            }
            let event_id_text = row.get::<_, Option<String>>(0)?.ok_or_else(|| {
                SemanticVectorStorePending::new("semantic vec0 event identity is malformed")
            })?;
            let event_id = Uuid::parse_str(&event_id_text)
                .context("invalid event id in semantic vec0 store")?;
            let source_text_hash = row.get::<_, Option<String>>(1)?.ok_or_else(|| {
                SemanticVectorStorePending::new("semantic vec0 source hash is malformed")
            })?;
            let start_char = row.get::<_, i64>(2)?.max(0) as usize;
            let end_char = row.get::<_, i64>(3)?.max(0) as usize;
            let embedding = row.get::<_, Vec<u8>>(4)?;
            exact_vector_bytes = exact_vector_bytes.saturating_add(embedding.len());
            if embedding.len() != semantic_exact_vector_bytes()
                || stats
                    .embedded_chunks
                    .saturating_mul(SEMANTIC_BINARY_VECTOR_BYTES)
                    .saturating_add(exact_vector_bytes)
                    > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES
            {
                return Err(SemanticVectorStorePending::new(
                    "sqlite vec0 exact rerank exceeded its trusted byte bound",
                )
                .into());
            }
            retain_best_semantic_chunk(
                &mut best_by_event,
                query_embedding,
                event_id,
                source_text_hash,
                start_char,
                end_char,
                &embedding,
            )?;
        }
        Ok(finish_semantic_search(
            best_by_event,
            limit,
            scan_started,
            stats.embedded_chunks,
            stats
                .embedded_chunks
                .saturating_mul(SEMANTIC_BINARY_VECTOR_BYTES)
                .saturating_add(exact_vector_bytes),
        ))
    }

    fn search_sqlite_vec0_event_ids(
        &self,
        query_embedding: &[f32],
        event_ids: &[Uuid],
        limit: usize,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        let scan_started = Instant::now();
        let slot = self
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .ok_or_else(|| SemanticVectorStorePending::new("projection slot is missing"))?;
        let placeholders = (0..event_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            r#"
            SELECT CASE WHEN length(m.event_id) = 36 THEN m.event_id END,
                   CASE WHEN length(m.source_text_sha256) = 64
                        THEN m.source_text_sha256 END,
                   m.start_char, m.end_char,
                   v.embedding
            FROM {SQLITE_VEC0_META_TABLE} AS m INDEXED BY {SQLITE_VEC0_EVENT_INDEX}
            CROSS JOIN {SQLITE_VEC0_TABLE} AS v
            WHERE m.slot = ? AND m.model_key = ?
              AND v.slot = ? AND v.model_key = ?
              AND v.rowid = m.rowid
              AND m.event_id IN ({placeholders})
            ORDER BY m.rowid
            LIMIT ?
            "#
        );
        let mut query_params = vec![
            SqlValue::from(slot),
            SqlValue::from(semantic_model_key().to_owned()),
            SqlValue::from(slot),
            SqlValue::from(semantic_model_key().to_owned()),
        ];
        query_params.extend(
            event_ids
                .iter()
                .map(|event_id| SqlValue::from(event_id.to_string())),
        );
        query_params.push(SqlValue::from(
            SEMANTIC_SQLITE_VEC0_MAX_K.saturating_add(1) as i64
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let mut exact_rows = 0_usize;
        let mut exact_vector_bytes = 0_usize;
        while let Some(row) = rows.next()? {
            if Instant::now() >= deadline {
                return Err(SemanticVectorStorePending::new(
                    "semantic vector retrieval deadline elapsed during filtered rerank",
                )
                .into());
            }
            exact_rows = exact_rows.saturating_add(1);
            let embedding = row.get::<_, Vec<u8>>(4)?;
            exact_vector_bytes = exact_vector_bytes.saturating_add(embedding.len());
            if exact_rows > SEMANTIC_SQLITE_VEC0_MAX_K
                || embedding.len() != semantic_exact_vector_bytes()
                || exact_vector_bytes > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES
            {
                return Err(SemanticVectorStorePending::new(
                    "filtered semantic rerank exceeded its trusted row or byte bound",
                )
                .into());
            }
            let event_id_text = row.get::<_, Option<String>>(0)?.ok_or_else(|| {
                SemanticVectorStorePending::new("semantic vec0 event identity is malformed")
            })?;
            let event_id = Uuid::parse_str(&event_id_text)
                .context("invalid event id in semantic vec0 store")?;
            let source_text_hash = row.get::<_, Option<String>>(1)?.ok_or_else(|| {
                SemanticVectorStorePending::new("semantic vec0 source hash is malformed")
            })?;
            retain_best_semantic_chunk(
                &mut best_by_event,
                query_embedding,
                event_id,
                source_text_hash,
                row.get::<_, i64>(2)?.max(0) as usize,
                row.get::<_, i64>(3)?.max(0) as usize,
                &embedding,
            )?;
        }
        Ok(finish_semantic_search(
            best_by_event,
            limit,
            scan_started,
            exact_rows,
            exact_vector_bytes,
        ))
    }
}
