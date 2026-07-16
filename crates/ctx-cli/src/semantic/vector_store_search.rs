fn semantic_streaming_scan_chunk_limit() -> usize {
    SEMANTIC_SQLITE_VEC0_MAX_K.min(ctx_protocol::SEARCH_MAX_CANDIDATE_ROWS)
}

fn semantic_streaming_scan_vector_byte_limit() -> usize {
    semantic_streaming_scan_chunk_limit()
        .saturating_mul(SEMANTIC_DIMENSIONS)
        .saturating_mul(std::mem::size_of::<f32>())
}

impl SemanticVectorStore {
    fn search(&self, query_embedding: &[f32], limit: usize) -> Result<SemanticVectorSearch> {
        self.search_until(
            query_embedding,
            limit,
            Instant::now() + SEMANTIC_VECTOR_SEARCH_TIMEOUT,
        )
    }

    fn search_until(
        &self,
        query_embedding: &[f32],
        limit: usize,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        self.search_with_event_filter(query_embedding, limit, None, deadline)
    }

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
        self.search_with_event_filter(query_embedding, limit, Some(event_ids), deadline)
    }

    fn search_with_event_filter(
        &self,
        query_embedding: &[f32],
        limit: usize,
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
        let result =
            self.search_with_event_filter_inner(query_embedding, limit, event_ids, deadline);
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
        event_ids: Option<&[Uuid]>,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
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
        if event_ids.is_none() && self.sqlite_vec0_search_ready()? {
            return self.search_sqlite_vec0(query_embedding, limit, stats, deadline);
        }
        if event_ids.is_none()
            && (stats.embedded_chunks > semantic_rust_full_scan_chunk_limit()
                || stats
                    .embedded_chunks
                    .saturating_mul(SEMANTIC_DIMENSIONS)
                    .saturating_mul(std::mem::size_of::<f32>())
                    > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES)
        {
            return Err(SemanticVectorStorePending::new(
                "trusted corpus exceeds the bounded blob fallback",
            )
            .into());
        }

        let scan_started = Instant::now();
        if Instant::now() >= deadline {
            return Err(SemanticVectorStorePending::new(
                "semantic vector retrieval deadline elapsed before blob scan",
            )
            .into());
        }
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Ok(SemanticVectorSearch {
                hits: Vec::new(),
                stats: SemanticVectorSearchStats {
                    backend: Some(SEMANTIC_VECTOR_BACKEND_RUST),
                    scan_ms: scan_started.elapsed().as_millis() as u64,
                    ..SemanticVectorSearchStats::default()
                },
            });
        }
        let mut sql = r#"
            SELECT event_id, source_text_sha256, start_char, end_char, embedding_f32
            FROM event_embedding_chunks
            WHERE model_key = ? AND dimensions = ?
            "#
        .to_owned();
        let chunk_cap = semantic_rust_full_scan_chunk_limit();
        let mut query_params = vec![
            SqlValue::from(semantic_model_key().to_owned()),
            SqlValue::from(SEMANTIC_DIMENSIONS as i64),
        ];
        if let Some(event_ids) = event_ids {
            let placeholders = (0..event_ids.len())
                .map(|_| "?")
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(" AND event_id IN (");
            sql.push_str(&placeholders);
            sql.push(')');
            query_params.extend(
                event_ids
                    .iter()
                    .map(|event_id| SqlValue::from(event_id.to_string())),
            );
            sql.push_str(" ORDER BY rowid LIMIT ?");
            query_params.push(SqlValue::from(chunk_cap.saturating_add(1) as i64));
        } else {
            sql.push_str(" ORDER BY rowid LIMIT ?");
            query_params.push(SqlValue::from(chunk_cap.saturating_add(1) as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let limit = limit.max(1);
        let mut chunks_scanned = 0_usize;
        let mut vector_bytes_read = 0_usize;
        while let Some(row) = rows.next()? {
            if Instant::now() >= deadline {
                return Err(SemanticVectorStorePending::new(
                    "semantic vector retrieval deadline elapsed during blob scan",
                )
                .into());
            }
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            let source_text_hash = row.get::<_, String>(1)?;
            let start_char = row.get::<_, i64>(2)?.max(0) as usize;
            let end_char = row.get::<_, i64>(3)?.max(0) as usize;
            let blob: Vec<u8> = row.get(4)?;
            chunks_scanned = chunks_scanned.saturating_add(1);
            vector_bytes_read = vector_bytes_read.saturating_add(blob.len());
            if chunks_scanned > chunk_cap
                || vector_bytes_read > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES
                || (event_ids.is_none() && chunks_scanned > stats.embedded_chunks)
            {
                return Err(SemanticVectorStorePending::new(
                    "semantic blob retrieval exceeded its trusted row or byte bound",
                )
                .into());
            }
            let Some(similarity) = dot_product_f32_blob(query_embedding, &blob)? else {
                continue;
            };
            match best_by_event.get_mut(&event_id) {
                Some(existing) if similarity > existing.similarity => {
                    *existing = SemanticVectorHit {
                        event_id,
                        similarity,
                        source_text_hash,
                        start_char,
                        end_char,
                    };
                }
                None => {
                    best_by_event.insert(
                        event_id,
                        SemanticVectorHit {
                            event_id,
                            similarity,
                            source_text_hash,
                            start_char,
                            end_char,
                        },
                    );
                }
                _ => {}
            }
        }
        if event_ids.is_none() && chunks_scanned != stats.embedded_chunks {
            return Err(SemanticVectorStorePending::new(
                "canonical rows drifted from trusted stats",
            )
            .into());
        }
        let events_scored = best_by_event.len();
        let mut top = best_by_event.into_values().collect::<Vec<_>>();
        if top.len() > limit {
            top.select_nth_unstable_by(limit - 1, compare_semantic_hits_desc);
            top.truncate(limit);
        }
        top.sort_by(compare_semantic_hits_desc);
        Ok(SemanticVectorSearch {
            hits: top,
            stats: SemanticVectorSearchStats {
                backend: Some(SEMANTIC_VECTOR_BACKEND_RUST),
                scan_ms: scan_started.elapsed().as_millis() as u64,
                chunks_scanned,
                vector_bytes_read,
                events_scored,
            },
        })
    }

    fn search_sqlite_vec0(
        &self,
        query_embedding: &[f32],
        limit: usize,
        stats: SemanticSidecarStats,
        deadline: Instant,
    ) -> Result<SemanticVectorSearch> {
        let scan_started = Instant::now();
        let streaming_chunk_limit = semantic_streaming_scan_chunk_limit();
        let streaming_vector_byte_limit = semantic_streaming_scan_vector_byte_limit();
        let corpus_vector_bytes = stats
            .embedded_chunks
            .saturating_mul(SEMANTIC_DIMENSIONS)
            .saturating_mul(std::mem::size_of::<f32>());
        if stats.embedded_chunks > streaming_chunk_limit
            || corpus_vector_bytes > streaming_vector_byte_limit
        {
            return Err(SemanticVectorStorePending::new(
                "trusted corpus exceeds the bounded sqlite vec0 streaming scan",
            )
            .into());
        }
        let query_blob = serialize_f32_blob(query_embedding);
        let slot = self
            .maintenance_state_i64(SQLITE_VEC0_ACTIVE_SLOT_STATE_KEY)?
            .ok_or_else(|| SemanticVectorStorePending::new("projection slot is missing"))?;
        let limit = limit.clamp(1, streaming_chunk_limit);
        let k = stats.embedded_chunks.max(1);
        if Instant::now() >= deadline {
            return Err(SemanticVectorStorePending::new(
                "semantic vector retrieval deadline elapsed before sqlite vec0 scan",
            )
            .into());
        }
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let mut rows_returned = 0_usize;
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.event_id, m.source_text_sha256, m.start_char, m.end_char, v.distance
            FROM event_embedding_vec0_v2 AS v
            JOIN event_embedding_vec0_meta_v2 AS m ON m.rowid = v.rowid
            WHERE v.slot = ?1
              AND v.model_key = ?2
              AND v.embedding MATCH ?3
              AND v.k = ?4
              AND m.slot = ?1
              AND m.model_key = ?2
            ORDER BY v.distance
            "#,
        )?;
        let mut rows = stmt.query(params![slot, semantic_model_key(), &query_blob, k as i64])?;
        while let Some(row) = rows.next()? {
            rows_returned = rows_returned.saturating_add(1);
            if rows_returned > stats.embedded_chunks || rows_returned > streaming_chunk_limit {
                return Err(SemanticVectorStorePending::new(
                    "sqlite vec0 returned candidates beyond its trusted corpus bound",
                )
                .into());
            }
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vec0 store")?;
            let source_text_hash = row.get::<_, String>(1)?;
            let start_char = row.get::<_, i64>(2)?.max(0) as usize;
            let end_char = row.get::<_, i64>(3)?.max(0) as usize;
            let distance = row.get::<_, f64>(4)? as f32;
            let similarity = (1.0 - distance).clamp(-1.0, 1.0);
            match best_by_event.get_mut(&event_id) {
                Some(existing) if similarity > existing.similarity => {
                    *existing = SemanticVectorHit {
                        event_id,
                        similarity,
                        source_text_hash,
                        start_char,
                        end_char,
                    };
                }
                None => {
                    best_by_event.insert(
                        event_id,
                        SemanticVectorHit {
                            event_id,
                            similarity,
                            source_text_hash,
                            start_char,
                            end_char,
                        },
                    );
                }
                _ => {}
            }
        }
        if rows_returned != stats.embedded_chunks || best_by_event.len() != stats.embedded_items {
            return Err(SemanticVectorStorePending::new(
                "sqlite vec0 candidates drifted from trusted current-model stats",
            )
            .into());
        }
        let events_scored = best_by_event.len();
        let mut hits = best_by_event.into_values().collect::<Vec<_>>();
        if hits.len() > limit {
            hits.select_nth_unstable_by(limit - 1, compare_semantic_hits_desc);
            hits.truncate(limit);
        }
        hits.sort_by(compare_semantic_hits_desc);
        Ok(SemanticVectorSearch {
            hits,
            stats: SemanticVectorSearchStats {
                backend: Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC),
                scan_ms: scan_started.elapsed().as_millis() as u64,
                chunks_scanned: stats.embedded_chunks,
                vector_bytes_read: corpus_vector_bytes,
                events_scored,
            },
        })
    }
}
