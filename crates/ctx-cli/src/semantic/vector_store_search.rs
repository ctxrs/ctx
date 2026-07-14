impl SemanticVectorStore {
    fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
        relational_revision: u64,
    ) -> Result<SemanticVectorSearch> {
        self.search_with_event_filter(query_embedding, limit, None, Some(relational_revision))
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
        self.search_with_event_filter(query_embedding, limit, Some(event_ids), None)
    }

    fn search_with_event_filter(
        &self,
        query_embedding: &[f32],
        limit: usize,
        event_ids: Option<&[Uuid]>,
        relational_revision: Option<u64>,
    ) -> Result<SemanticVectorSearch> {
        let owns_snapshot = self.conn.is_autocommit();
        if owns_snapshot {
            self.conn.execute_batch("BEGIN DEFERRED")?;
        }
        let result = self.search_with_event_filter_snapshot(
            query_embedding,
            limit,
            event_ids,
            relational_revision,
        );
        if !owns_snapshot {
            return result;
        }
        match result {
            Ok(search) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(search)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn search_with_event_filter_snapshot(
        &self,
        query_embedding: &[f32],
        limit: usize,
        event_ids: Option<&[Uuid]>,
        relational_revision: Option<u64>,
    ) -> Result<SemanticVectorSearch> {
        if event_ids.is_none() {
            let relational_revision = relational_revision
                .ok_or_else(|| anyhow!("full semantic search requires a relational revision"))?;
            if self.sqlite_vec0_search_ready(relational_revision)? {
                return self.handle_sqlite_vec0_query_result(
                    self.search_sqlite_vec0(query_embedding, limit),
                );
            }
            let exact = self.exact_stats()?;
            if exact.embedded_chunks > semantic_rust_full_scan_chunk_limit() {
                return Err(anyhow!(
                    "semantic vec0 index is unavailable or pending repair; complete CPU scan would exceed the local pressure limit"
                ));
            }
        }

        let scan_started = Instant::now();
        if !sqlite_table_exists(&self.conn, "event_embedding_chunks")? {
            return Err(anyhow!(
                "semantic vector store is missing event_embedding_chunks; daemon repair is required"
            ));
        }
        let mut sql = r#"
            SELECT event_id, source_text_sha256, start_char, end_char, embedding_f32
            FROM event_embedding_chunks
            WHERE model_key = ?1
              AND dimensions = ?2
            "#
        .to_owned();
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
        } else {
            sql.push_str(" ORDER BY event_seq DESC");
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
                return Err(anyhow!(
                    "complete semantic CPU scan exceeded the local pressure limit"
                ));
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
        let generation = self
            .maintenance_state_i64(SEMANTIC_VEC0_ACTIVE_GENERATION_KEY)?
            .ok_or_else(|| anyhow!("semantic vec0 has no active generation"))?;
        let (vec_table, meta_table) = sqlite_vec0_generation_tables(generation)
            .ok_or_else(|| anyhow!("semantic vec0 active generation is invalid"))?;
        let scan_started = Instant::now();
        let query_blob = serialize_f32_blob(query_embedding);
        let limit = limit.max(1);
        let exact_chunks = self.exact_chunk_count()?;
        let max_k = limit
            .saturating_mul(SEMANTIC_MAX_CHUNKS_PER_DOCUMENT)
            .min(exact_chunks.max(1))
            .max(limit.min(exact_chunks.max(1)));
        let mut k = limit.min(max_k);
        let mut best_by_event = HashMap::<Uuid, SemanticVectorHit>::new();
        let mut rows_returned: usize;
        loop {
            best_by_event.clear();
            rows_returned = 0;
            let mut stmt = self.conn.prepare(&format!(
                r#"
                SELECT m.event_id, m.source_text_sha256, m.start_char, m.end_char, v.distance
                FROM {vec_table} AS v
                JOIN {meta_table} AS m ON m.rowid = v.rowid
	                WHERE v.embedding MATCH ?1
	                  AND v.k = ?2
	                  AND m.model_key = ?3
	                ORDER BY v.distance
	                "#
            ))?;
            let mut rows = stmt.query(params![&query_blob, k as i64, semantic_model_key()])?;
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
            && max_k < exact_chunks
        {
            return Err(anyhow!(
                "semantic vec0 contains more chunks per document than the canonical bound; daemon repair is required"
            ));
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
                chunks_scanned: rows_returned,
                vector_bytes_read: rows_returned
                    .saturating_mul(SEMANTIC_DIMENSIONS)
                    .saturating_mul(std::mem::size_of::<f32>()),
                events_scored,
            },
        })
    }

    fn handle_sqlite_vec0_query_result(
        &self,
        result: Result<SemanticVectorSearch>,
    ) -> Result<SemanticVectorSearch> {
        match result {
            Ok(search) => Ok(search),
            Err(error) => {
                request_semantic_vector_repair(&self.path).with_context(|| {
                    format!(
                        "semantic vec0 query failed ({error:#}) and its daemon repair request could not be persisted"
                    )
                })?;
                Err(error).context("semantic vec0 query failed; daemon repair has been scheduled")
            }
        }
    }
}

fn semantic_vector_repair_request_path(vector_path: &Path) -> PathBuf {
    vector_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(SEMANTIC_VECTOR_REPAIR_REQUEST_FILE)
}

fn request_semantic_vector_repair(vector_path: &Path) -> Result<()> {
    write_private_json_file(
        &semantic_vector_repair_request_path(vector_path),
        &json!({
            "schema_version": 1,
            "requested_at_ms": utc_now().timestamp_millis(),
            "reason": "vec0_query_failure",
        }),
    )
}
