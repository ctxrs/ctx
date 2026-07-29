pub(super) struct SemanticVectorHit {
    pub(super) event_id: Uuid,
    pub(super) similarity: f32,
    pub(super) source_text_hash: String,
    pub(super) start_char: usize,
    pub(super) end_char: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SemanticVectorSearchStats {
    pub(super) backend: Option<&'static str>,
    pub(super) scan_ms: u64,
    pub(super) chunks_scanned: usize,
    pub(super) vector_bytes_read: usize,
    pub(super) events_scored: usize,
}

#[derive(Default)]
pub(super) struct SemanticVectorSearch {
    pub(super) hits: Vec<SemanticVectorHit>,
    pub(super) stats: SemanticVectorSearchStats,
}

pub(super) struct SemanticHitSearch {
    pub(super) hits: Vec<ctx_history_search::SemanticEventHit>,
    pub(super) diagnostics: SemanticRetrievalDiagnostics,
}

#[derive(Debug, Clone)]
pub(super) struct SemanticChunkDocument {
    pub(super) event_id: Uuid,
    pub(super) seq: u64,
    pub(super) chunk_index: usize,
    pub(super) source_text_hash: String,
    pub(super) text: String,
    pub(super) start_char: usize,
    pub(super) end_char: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SemanticSidecarStats {
    pub(super) embedded_items: usize,
    pub(super) embedded_chunks: usize,
}

#[derive(Debug, Clone)]
pub(super) struct SemanticStoredEvent {
    pub(super) event_id: Uuid,
    pub(super) source_text_hash: String,
    pub(super) seq: u64,
}

#[derive(Debug, Default)]
pub(super) struct SemanticIndexOutcome {
    pub(super) indexed_chunks: usize,
    pub(super) consumed_event_ids: Vec<Uuid>,
}

#[derive(Debug, Default)]
pub(super) struct SemanticPruneOutcome {
    pub(super) scanned_events: usize,
    pub(super) deleted_chunks: usize,
    pub(super) queued_stale_events: usize,
    pub(super) scan_complete: bool,
}

pub(super) struct SemanticVectorStore {
    pub(super) conn: Connection,
    pub(super) flat: flat_segments::FlatSegmentStore,
}
use rusqlite::Connection;
use uuid::Uuid;

use super::reports::SemanticRetrievalDiagnostics;

mod source_projection;
pub(super) use source_projection::{
    semantic_hydrated_source_is_control, source_backed_semantic_vector_path,
    SourceBackedSemanticEmbedder, SourceBackedSemanticOutcome, SourceBackedSemanticResolver,
};
pub(super) mod control;
pub(super) mod flat_scan;
pub(super) mod flat_segments;
