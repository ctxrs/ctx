use std::{
    collections::HashSet,
    sync::{Condvar, LazyLock, Mutex},
    time::Instant,
};

use anyhow::Result;
use uuid::Uuid;

use super::{
    model_contract::SEMANTIC_DIMENSIONS,
    runtime_limits::{SEMANTIC_EXACT_QUERY_CONCURRENCY, SEMANTIC_EXACT_TOP_K_MAX},
    vector_store::{
        flat_scan::{
            ActiveChunk, ExactFlatF32Scan, FlatScanConfig, FlatScanLocation, FlatScanSkipReason,
        },
        flat_segments::PinnedFlatGeneration,
        SemanticVectorHit, SemanticVectorSearch, SemanticVectorSearchStats, SemanticVectorStore,
    },
    vector_store_schema::{
        semantic_owned_sidecar_result, SemanticVectorStoreError, SEMANTIC_VECTOR_BACKEND_FLAT_F32,
    },
};

static EXACT_QUERY_LIMITER: LazyLock<ExactQueryLimiter> = LazyLock::new(ExactQueryLimiter::default);

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
        if limit > SEMANTIC_EXACT_TOP_K_MAX {
            return Err(SemanticVectorStoreError::unavailable(format!(
                "semantic top-k {limit} exceeds the exact-scan cap {SEMANTIC_EXACT_TOP_K_MAX}"
            ))
            .into());
        }
        if limit == 0 {
            return Ok(SemanticVectorSearch::default());
        }
        let _permit = EXACT_QUERY_LIMITER.acquire();
        let started = Instant::now();
        let Some(reader) = self.flat_pin_generation()? else {
            return Ok(SemanticVectorSearch {
                hits: Vec::new(),
                stats: SemanticVectorSearchStats {
                    backend: Some(SEMANTIC_VECTOR_BACKEND_FLAT_F32),
                    scan_ms: started.elapsed().as_millis() as u64,
                    ..SemanticVectorSearchStats::default()
                },
            });
        };
        semantic_owned_sidecar_result(scan_exact_generation(
            &reader,
            query_embedding,
            limit,
            None,
            started,
        ))
    }
}

pub(super) fn scan_exact_generation(
    reader: &PinnedFlatGeneration,
    query_embedding: &[f32],
    limit: usize,
    allowed_events: Option<&HashSet<Uuid>>,
    started: Instant,
) -> Result<SemanticVectorSearch> {
    let dimensions = usize::try_from(reader.model_contract().dimensions)?;
    let mut scan = ExactFlatF32Scan::new(query_embedding, FlatScanConfig::new(dimensions, limit))
        .map_err(|error| SemanticVectorStoreError::unavailable(error.to_string()))?;

    if let Some(allowed) = allowed_events {
        for (segment_index, segment) in reader.scan_segments().iter().enumerate() {
            let mut chunks = segment.scoring_chunks().peekable();
            while let Some(chunk) = chunks.next() {
                if allowed.contains(&chunk.event_id) {
                    scan.scan_prevalidated_f32(std::iter::once((
                        ActiveChunk::at_location(
                            chunk.event_id,
                            chunk.chunk_index,
                            FlatScanLocation {
                                segment_index,
                                segment_ordinal: chunk.ordinal,
                            },
                        ),
                        chunk.vector,
                    )))
                    .map_err(|error| SemanticVectorStoreError::unavailable(error.to_string()))?;
                    continue;
                }
                let event_id = chunk.event_id;
                let mut skipped = 1_usize;
                while chunks.peek().is_some_and(|next| next.event_id == event_id) {
                    let _ = chunks.next();
                    skipped = skipped.saturating_add(1);
                }
                scan.skip_event(skipped, FlatScanSkipReason::Filtered)
                    .map_err(|error| SemanticVectorStoreError::unavailable(error.to_string()))?;
            }
        }
    } else {
        for (segment_index, segment) in reader.scan_segments().iter().enumerate() {
            scan.scan_prevalidated_f32(segment.scoring_chunks().map(|chunk| {
                (
                    ActiveChunk::at_location(
                        chunk.event_id,
                        chunk.chunk_index,
                        FlatScanLocation {
                            segment_index,
                            segment_ordinal: chunk.ordinal,
                        },
                    ),
                    chunk.vector,
                )
            }))
            .map_err(|error| SemanticVectorStoreError::unavailable(error.to_string()))?;
        }
    }
    let scanned = scan
        .finish()
        .map_err(|error| SemanticVectorStoreError::unavailable(error.to_string()))?;
    let mut hits = Vec::with_capacity(scanned.hits.len());
    for hit in &scanned.hits {
        let chunk = hit
            .location
            .and_then(|location| {
                reader
                    .scan_segments()
                    .get(location.segment_index)
                    .and_then(|segment| segment.chunk_at(location.segment_ordinal))
            })
            .filter(|chunk| {
                chunk.event_id == hit.event_id && chunk.chunk_index == hit.chunk_ordinal
            })
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic exact-scan winner metadata is missing from its pinned generation",
                )
            })?;
        hits.push(SemanticVectorHit {
            event_id: chunk.event_id,
            similarity: hit.similarity,
            source_text_hash: chunk.source_text_hash.to_hex(),
            start_char: chunk.start_char as usize,
            end_char: chunk.end_char as usize,
        });
    }
    hits.sort_by(|left, right| {
        right
            .similarity
            .total_cmp(&left.similarity)
            .then_with(|| left.event_id.cmp(&right.event_id))
    });
    hits.truncate(limit);
    Ok(SemanticVectorSearch {
        hits,
        stats: SemanticVectorSearchStats {
            backend: Some(SEMANTIC_VECTOR_BACKEND_FLAT_F32),
            scan_ms: started.elapsed().as_millis() as u64,
            chunks_scanned: scanned.counters.chunks_scanned,
            vector_bytes_read: scanned.counters.vector_bytes_read,
            events_scored: scanned.counters.events_scored,
        },
    })
}

#[derive(Default)]
struct ExactQueryLimiter {
    active: Mutex<usize>,
    wake: Condvar,
}

impl ExactQueryLimiter {
    fn acquire(&self) -> ExactQueryPermit<'_> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *active >= SEMANTIC_EXACT_QUERY_CONCURRENCY {
            active = self
                .wake
                .wait(active)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *active = active.saturating_add(1);
        ExactQueryPermit { limiter: self }
    }
}

struct ExactQueryPermit<'a> {
    limiter: &'a ExactQueryLimiter,
}

impl Drop for ExactQueryPermit<'_> {
    fn drop(&mut self) {
        let mut active = self
            .limiter
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        self.limiter.wake.notify_one();
    }
}
