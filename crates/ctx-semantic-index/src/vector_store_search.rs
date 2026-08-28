use std::{
    sync::{Condvar, LazyLock, Mutex},
    time::Instant,
};

use anyhow::Result;
use uuid::Uuid;

use super::{
    vector_store::{
        flat_scan::{
            ActiveChunk, ExactFlatF32Scan, FlatScanConfig, FlatScanLocation, FlatScanSkipReason,
        },
        flat_segments::PinnedFlatGeneration,
        SemanticVectorHit, SemanticVectorSearch, SemanticVectorSearchStats,
    },
    vector_store_schema::{SemanticVectorStoreError, SEMANTIC_VECTOR_BACKEND_FLAT_F32},
};

const SEMANTIC_EXACT_QUERY_CONCURRENCY: usize = 2;
pub(super) const SEMANTIC_EXACT_TOP_K_MAX: usize = 4_096;

static EXACT_QUERY_LIMITER: LazyLock<ExactQueryLimiter> = LazyLock::new(ExactQueryLimiter::default);

pub(super) fn scan_exact_generation(
    reader: &PinnedFlatGeneration,
    query_embeddings: &[Vec<f32>],
    limit: usize,
    event_identity_digest: &dyn Fn(Uuid) -> Option<[u8; 32]>,
    started: Instant,
) -> Result<SemanticVectorSearch> {
    if query_embeddings.is_empty() {
        return Err(SemanticVectorStoreError::unavailable(
            "semantic exact scan requires at least one query vector",
        )
        .into());
    }
    let dimensions = usize::try_from(reader.model_contract().dimensions)?;
    for (query_ordinal, query_embedding) in query_embeddings.iter().enumerate() {
        if query_embedding.len() != dimensions {
            return Err(SemanticVectorStoreError::unavailable(format!(
                "semantic query alternative {query_ordinal} has {} dimensions, expected {dimensions}",
                query_embedding.len()
            ))
            .into());
        }
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
    let query_vectors = query_embeddings
        .iter()
        .map(Vec::as_slice)
        .collect::<Vec<_>>();
    let mut scan =
        ExactFlatF32Scan::new_multi(&query_vectors, FlatScanConfig::new(dimensions, limit))
            .map_err(|error| SemanticVectorStoreError::unavailable(error.to_string()))?;

    for (segment_index, segment) in reader.scan_segments().iter().enumerate() {
        let mut chunks = segment.scoring_chunks().peekable();
        while let Some(chunk) = chunks.next() {
            if let Some(event_identity_digest) = event_identity_digest(chunk.event_id) {
                scan.scan_prevalidated_f32(std::iter::once((
                    ActiveChunk::at_location(
                        chunk.event_id,
                        event_identity_digest,
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
            event_identity_digest: hit.event_identity_digest,
            similarity: hit.similarity,
            query_ordinal: hit.query_ordinal,
            source_text_hash: chunk.source_text_hash.to_hex(),
            start_char: chunk.start_char as usize,
            end_char: chunk.end_char as usize,
        });
    }
    debug_assert!(hits.len() <= limit);
    Ok(SemanticVectorSearch {
        hits,
        stats: SemanticVectorSearchStats {
            backend: Some(SEMANTIC_VECTOR_BACKEND_FLAT_F32),
            scan_ms: started.elapsed().as_millis() as u64,
            chunks_scanned: scanned.counters.chunks_scanned,
            vector_bytes_read: scanned.counters.vector_bytes_read,
            events_scored: scanned.counters.events_scored,
            query_vectors: query_embeddings.len(),
            vector_passes: 1,
            dot_products: scanned.counters.dot_products,
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

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use ctx_semantic_model::semantic_model_contract;

    use super::*;
    use crate::vector_store::{
        flat_segments::{
            FlatChunk, FlatEventReplacement, FlatModelContract, FlatSegmentStore, FlatSourceHash,
        },
        SemanticChunkDocument, SemanticVectorStore,
    };

    #[test]
    fn direct_exact_generation_scan_validates_pinned_contract_dimensions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = FlatSegmentStore::open(
            temp.path(),
            FlatModelContract {
                contract_version: 2,
                model_id: "test/non-builtin-dimensions".to_owned(),
                model_revision: "revision-1".to_owned(),
                tokenizer: "tokenizer-sha256".to_owned(),
                pooling: "attention-mask-mean".to_owned(),
                dimensions: 4,
                normalization: "l2".to_owned(),
            },
        )?;
        let event_id = Uuid::new_v4();
        let event_identity_digest = [7; 32];
        store.publish_replacement_event_chunks(
            &[FlatEventReplacement {
                event_id,
                seq: 1,
                source_text_hash: FlatSourceHash::from_bytes([8; 32]),
                chunks: vec![FlatChunk {
                    chunk_index: 0,
                    start_char: 0,
                    end_char: 1,
                    vector: vec![1.0, 0.0, 0.0, 0.0],
                }],
            }],
            &[],
        )?;
        let pinned = store
            .pin_generation()?
            .expect("fixture must publish a flat generation");

        let error = scan_exact_generation(
            &pinned,
            &[vec![1.0, 0.0, 0.0]],
            1,
            &|candidate| (candidate == event_id).then_some(event_identity_digest),
            Instant::now(),
        )
        .err()
        .expect("the query must match the pinned flat generation dimensions");

        assert_eq!(
            error.to_string(),
            "semantic query alternative 0 has 3 dimensions, expected 4"
        );
        Ok(())
    }

    #[test]
    fn direct_exact_generation_scan_waits_for_bounded_admission() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let contract = semantic_model_contract();
        let mut store =
            SemanticVectorStore::open(&temp.path().join("search").join("semantic"), contract)?;
        let event_id = Uuid::new_v4();
        let event_identity_digest = [7; 32];
        let mut embedding = vec![0.0; contract.dimensions()];
        embedding[0] = 1.0;
        store.publish_chunk_replacements(
            &[(
                SemanticChunkDocument {
                    event_id,
                    seq: 1,
                    chunk_index: 0,
                    source_text_hash: "00".repeat(32),
                    text: String::new(),
                    start_char: 0,
                    end_char: 1,
                },
                embedding.clone(),
            )],
            &[],
        )?;
        let pinned = store
            .flat_pin_generation()?
            .expect("fixture must publish a flat generation");
        let permits = (0..SEMANTIC_EXACT_QUERY_CONCURRENCY)
            .map(|_| EXACT_QUERY_LIMITER.acquire())
            .collect::<Vec<_>>();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ = entered_tx.send(());
            let result = scan_exact_generation(
                &pinned,
                std::slice::from_ref(&embedding),
                1,
                &|candidate| (candidate == event_id).then_some(event_identity_digest),
                Instant::now(),
            );
            let _ = result_tx.send(result);
        });

        entered_rx.recv_timeout(Duration::from_secs(1))?;
        assert!(matches!(
            result_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(permits);
        let search = result_rx.recv_timeout(Duration::from_secs(2))??;
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("exact-scan admission test thread panicked"))?;
        assert_eq!(search.hits.len(), 1);
        assert_eq!(search.hits[0].event_id, event_id);
        Ok(())
    }
}
