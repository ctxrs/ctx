impl SemanticVectorStore {
    fn search(&self, query_embedding: &[f32], limit: usize) -> Result<SemanticVectorSearch> {
        self.search_with_event_filter(query_embedding, limit, None)
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
        self.search_with_event_filter(query_embedding, limit, Some(event_ids))
    }

    fn search_with_event_filter(
        &self,
        query_embedding: &[f32],
        limit: usize,
        event_ids: Option<&[Uuid]>,
    ) -> Result<SemanticVectorSearch> {
        if event_ids.is_none() && self.sqlite_vec0_search_ready()? {
            if let Ok(search) = self.search_sqlite_vec0(query_embedding, limit) {
                return Ok(search);
            }
        }

        let scan_started = Instant::now();
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
            WHERE model_key = ?1
              AND dimensions = ?2
            "#
        .to_owned();
        let mut query_params = vec![
            SqlValue::from(SEMANTIC_MODEL_KEY.to_owned()),
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
        } else {
            sql.push_str(" ORDER BY event_seq DESC LIMIT ?");
            query_params.push(SqlValue::from(SEMANTIC_FULL_SCAN_MAX_CHUNKS as i64));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(query_params))?;
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let limit = limit.max(1);
        let mut chunks_scanned = 0_usize;
        let mut vector_bytes_read = 0_usize;
        while let Some(row) = rows.next()? {
            let event_id = Uuid::parse_str(&row.get::<_, String>(0)?)
                .context("invalid event id in semantic vector store")?;
            let source_text_hash = row.get::<_, String>(1)?;
            let start_char = row.get::<_, i64>(2)?.max(0) as usize;
            let end_char = row.get::<_, i64>(3)?.max(0) as usize;
            let blob: Vec<u8> = row.get(4)?;
            chunks_scanned = chunks_scanned.saturating_add(1);
            vector_bytes_read = vector_bytes_read.saturating_add(blob.len());
            if event_ids.is_none() && vector_bytes_read > SEMANTIC_FULL_SCAN_MAX_VECTOR_BYTES {
                break;
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
    ) -> Result<SemanticVectorSearch> {
        let scan_started = Instant::now();
        let query_blob = serialize_f32_blob(query_embedding);
        let stats = self.cached_or_exact_stats()?;
        let limit = limit.clamp(1, SEMANTIC_SQLITE_VEC0_MAX_K);
        let max_k = stats
            .embedded_chunks
            .max(limit)
            .clamp(1, SEMANTIC_SQLITE_VEC0_MAX_K);
        let mut k = limit.min(max_k);
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let mut rows_returned: usize;
        loop {
            best_by_event.clear();
            rows_returned = 0;
            let mut stmt = self.conn.prepare(
                r#"
                SELECT m.event_id, m.source_text_sha256, m.start_char, m.end_char, v.distance
                FROM event_embedding_vec0 AS v
                JOIN event_embedding_vec0_meta AS m ON m.rowid = v.rowid
	                WHERE v.embedding MATCH ?1
	                  AND v.k = ?2
	                  AND m.model_key = ?3
	                ORDER BY v.distance
	                "#,
            )?;
            let mut rows = stmt.query(params![&query_blob, k as i64, SEMANTIC_MODEL_KEY])?;
            while let Some(row) = rows.next()? {
                rows_returned = rows_returned.saturating_add(1);
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
            if best_by_event.len() >= limit || rows_returned < k || k >= max_k {
                break;
            }
            k = k.saturating_mul(2).min(max_k);
        }
        if best_by_event.len() < limit
            && rows_returned >= k
            && k >= max_k
            && max_k < stats.embedded_chunks
        {
            return Err(anyhow!(
                "sqlite vec0 top-k cap reached before enough unique semantic events"
            ));
        }
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
                vector_bytes_read: stats
                    .embedded_chunks
                    .saturating_mul(SEMANTIC_DIMENSIONS)
                    .saturating_mul(std::mem::size_of::<f32>()),
                events_scored: stats.embedded_items,
            },
        })
    }
}
