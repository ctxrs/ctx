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

#[derive(Debug, Clone)]
pub(super) struct SemanticStoredEvent {
    pub(super) event_id: Uuid,
    pub(super) source_text_hash: String,
    pub(super) seq: u64,
}

pub(super) struct SemanticVectorStore {
    pub(super) conn: Connection,
    pub(super) flat: flat_segments::FlatSegmentStore,
}
use rusqlite::Connection;
use uuid::Uuid;

mod source_projection;
pub(super) use source_projection::{
    semantic_hydrated_source_is_control, source_backed_semantic_vector_path,
    SourceBackedSemanticEmbedder, SourceBackedSemanticOutcome, SourceBackedSemanticResolver,
};
pub(super) mod control;
pub(super) mod flat_scan;
pub(super) mod flat_segments;
