use std::{cmp::Ordering, collections::HashMap, time::Instant};

use anyhow::Result;
use rusqlite::params;
use uuid::Uuid;

use super::{
    indexing::serialize_f32_blob,
    model_contract::SEMANTIC_DIMENSIONS,
    vector_store::{
        SemanticVectorHit, SemanticVectorSearch, SemanticVectorSearchStats, SemanticVectorStore,
    },
    vector_store_schema::{
        semantic_owned_sidecar_result, SemanticVectorStoreError,
        SEMANTIC_SQLITE_VEC0_INITIAL_OVERFETCH_DIVISOR, SEMANTIC_SQLITE_VEC0_MAX_K,
        SEMANTIC_VECTOR_BACKEND_SQLITE_VEC,
    },
};

impl SemanticVectorStore {
    pub(super) fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<SemanticVectorSearch> {
        if query_embedding.len() != SEMANTIC_DIMENSIONS {
            return Err(SemanticVectorStoreError::unavailable(format!(
                "semantic query has {} dimensions, expected {SEMANTIC_DIMENSIONS}",
                query_embedding.len()
            ))
            .into());
        }
        if limit > SEMANTIC_SQLITE_VEC0_MAX_K {
            return Err(SemanticVectorStoreError::unavailable(format!(
                "semantic top-k {limit} exceeds the safe sqlite-vec cap {SEMANTIC_SQLITE_VEC0_MAX_K}"
            ))
            .into());
        }
        if limit == 0 {
            return Ok(SemanticVectorSearch::default());
        }
        semantic_owned_sidecar_result(self.search_sqlite_vec0(query_embedding, limit))
    }

    fn search_sqlite_vec0(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<SemanticVectorSearch> {
        let started = Instant::now();
        let stats = self.cached_or_exact_stats()?;
        if stats.embedded_chunks == 0 {
            return Ok(SemanticVectorSearch {
                hits: Vec::new(),
                stats: SemanticVectorSearchStats {
                    backend: Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC),
                    scan_ms: started.elapsed().as_millis() as u64,
                    ..SemanticVectorSearchStats::default()
                },
            });
        }
        let (mut k, maximum_k) = semantic_sqlite_vec0_query_bounds(limit, stats.embedded_chunks);
        let query = serialize_f32_blob(query_embedding);
        let mut best = HashMap::<Uuid, (SemanticVectorHit, i64)>::new();
        let mut total_rows = 0;
        let final_rows;
        loop {
            best.clear();
            let mut rows_returned = 0;
            let mut statement = self.conn.prepare(
                "SELECT m.event_id, m.source_text_sha256, m.start_char, m.end_char,
                        v.distance, m.chunk_id
                 FROM event_embedding_vec0 AS v
                 JOIN event_embedding_chunks AS m ON m.chunk_id = v.rowid
                 WHERE v.embedding MATCH ?1 AND v.k = ?2
                 ORDER BY v.distance, m.chunk_id",
            )?;
            let mut rows = statement.query(params![&query, i64::try_from(k)?])?;
            while let Some(row) = rows.next()? {
                rows_returned += 1;
                total_rows += 1;
                let event_id_text = row.get::<_, String>(0)?;
                let event_id = Uuid::parse_str(&event_id_text).map_err(|_| {
                    SemanticVectorStoreError::reset_required(format!(
                        "invalid event id in semantic vector store; manual rebuild required: {event_id_text}"
                    ))
                })?;
                let similarity = (1.0 - row.get::<_, f64>(4)? as f32).clamp(-1.0, 1.0);
                let chunk_id = row.get::<_, i64>(5)?;
                let replace = best.get(&event_id).is_none_or(|(hit, existing_chunk_id)| {
                    similarity > hit.similarity
                        || (similarity == hit.similarity && chunk_id < *existing_chunk_id)
                });
                if replace {
                    best.insert(
                        event_id,
                        (
                            SemanticVectorHit {
                                event_id,
                                similarity,
                                source_text_hash: row.get(1)?,
                                start_char: usize::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                                end_char: usize::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                            },
                            chunk_id,
                        ),
                    );
                }
            }
            if best.len() >= limit || rows_returned < k || k == maximum_k {
                final_rows = rows_returned;
                break;
            }
            k = k.saturating_mul(2).min(maximum_k);
        }
        if final_rows < k && k <= stats.embedded_chunks && best.len() < limit {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic vec0 rows no longer match v7 metadata; manual rebuild required",
            )
            .into());
        }
        let events_scored = best.len();
        let mut hits = best.into_values().map(|value| value.0).collect::<Vec<_>>();
        hits.sort_by(compare_hits);
        hits.truncate(limit);
        Ok(SemanticVectorSearch {
            hits,
            stats: SemanticVectorSearchStats {
                backend: Some(SEMANTIC_VECTOR_BACKEND_SQLITE_VEC),
                scan_ms: started.elapsed().as_millis() as u64,
                chunks_scanned: total_rows,
                vector_bytes_read: total_rows
                    .saturating_mul(SEMANTIC_DIMENSIONS)
                    .saturating_mul(std::mem::size_of::<f32>()),
                events_scored,
            },
        })
    }
}

pub(super) fn semantic_sqlite_vec0_query_bounds(
    limit: usize,
    embedded_chunks: usize,
) -> (usize, usize) {
    let maximum_k = embedded_chunks.min(SEMANTIC_SQLITE_VEC0_MAX_K);
    if maximum_k == 0 {
        return (0, 0);
    }
    let initial_k = limit
        .saturating_add(limit.div_ceil(SEMANTIC_SQLITE_VEC0_INITIAL_OVERFETCH_DIVISOR))
        .clamp(1, maximum_k);
    (initial_k, maximum_k)
}

fn compare_hits(left: &SemanticVectorHit, right: &SemanticVectorHit) -> Ordering {
    right
        .similarity
        .total_cmp(&left.similarity)
        .then_with(|| left.event_id.cmp(&right.event_id))
}
