pub(super) struct SemanticVectorHit {
    pub(super) event_id: Uuid,
    pub(super) similarity: f32,
    #[cfg_attr(not(test), expect(dead_code))]
    pub(super) source_text_hash: String,
    #[expect(dead_code)]
    pub(super) start_char: usize,
    #[expect(dead_code)]
    pub(super) end_char: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SemanticVectorSearchStats {
    #[cfg_attr(not(test), expect(dead_code))]
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
pub struct SemanticChunkDocument {
    pub(super) event_id: Uuid,
    pub(super) seq: u64,
    pub(super) chunk_index: usize,
    pub(super) source_text_hash: String,
    pub(super) text: String,
    pub(super) start_char: usize,
    pub(super) end_char: usize,
}

impl SemanticChunkDocument {
    pub fn text(&self) -> &str {
        &self.text
    }
}

pub struct SemanticVectorStore {
    pub(super) conn: Connection,
    pub(super) flat: flat_segments::FlatSegmentStore,
}
use rusqlite::Connection;
use uuid::Uuid;

mod source_projection;
pub use flat_segments::PinnedFlatGeneration;
pub use source_projection::{
    semantic_core_content_is_control, source_backed_semantic_vector_path, SemanticBatchEmbedder,
    SemanticDocumentBuilder, SourceBackedGenerationPin, SourceBackedSemanticOutcome,
};
pub(super) mod control;
pub(super) mod flat_scan;
pub(super) mod flat_segments;
