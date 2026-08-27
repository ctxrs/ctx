//! Generation-pinned Flat-F32 semantic projection, persistence, recovery, and query.
//!
//! This crate consumes embeddings supplied by its callers. Model acquisition,
//! runtime loading, E5 identity, and embedding generation remain owned by
//! `ctx-semantic-model`.

mod document;
mod indexing;
mod json;
mod private_fs;
mod query_index;
mod source_document;
mod vector_store;
mod vector_store_schema;
mod vector_store_search;
mod vector_store_state;

pub use document::SemanticEventDocument;
pub use query_index::{SemanticNotReady, SemanticQueryPin};
pub use source_document::SourceBackedSemanticDocumentBuilder;
pub use vector_store::{
    semantic_core_content_is_control, source_backed_semantic_contract_fingerprint,
    source_backed_semantic_vector_path, PinnedFlatGeneration, SemanticBatchEmbedder,
    SemanticChunkDocument, SemanticDocumentBuilder, SemanticVectorStore, SourceBackedGenerationPin,
    SourceBackedSemanticOutcome,
};
pub use vector_store_schema::{semantic_vector_failure_kind, SemanticVectorFailureKind};

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use anyhow::{anyhow, Result};
    use uuid::Uuid;

    use super::{
        query_index::SemanticQueryPin,
        vector_store::{flat_segments::PinnedFlatGeneration, SemanticChunkDocument},
        vector_store_schema::SemanticVectorStoreError,
        SemanticVectorStore, SourceBackedGenerationPin,
    };

    #[allow(clippy::too_many_arguments)]
    pub fn semantic_chunk_document(
        event_id: Uuid,
        seq: u64,
        chunk_index: usize,
        source_text_hash: String,
        text: String,
        start_char: usize,
        end_char: usize,
    ) -> SemanticChunkDocument {
        SemanticChunkDocument {
            event_id,
            seq,
            chunk_index,
            source_text_hash,
            text,
            start_char,
            end_char,
        }
    }

    pub fn publish_chunk_replacements(
        store: &mut SemanticVectorStore,
        chunks: &[(SemanticChunkDocument, Vec<f32>)],
        deleted_event_ids: &[Uuid],
    ) -> Result<usize> {
        store.publish_chunk_replacements(chunks, deleted_event_ids)
    }

    pub fn pinned_flat_generation(store: &SemanticVectorStore) -> Result<PinnedFlatGeneration> {
        store
            .flat_pin_generation()?
            .ok_or_else(|| anyhow!("semantic test store has no flat generation"))
    }

    pub fn semantic_query_pin(
        core_generation_id: &str,
        readiness: SourceBackedGenerationPin,
    ) -> Result<SemanticQueryPin> {
        SemanticQueryPin::from_readiness_for_test(core_generation_id, readiness)
    }

    pub fn storage_conflict_error(message: impl Into<String>) -> SemanticVectorStoreError {
        SemanticVectorStoreError::storage_conflict(message)
    }

    pub fn reset_required_error(message: impl Into<String>) -> SemanticVectorStoreError {
        SemanticVectorStoreError::reset_required(message)
    }

    pub fn unavailable_error(message: impl Into<String>) -> SemanticVectorStoreError {
        SemanticVectorStoreError::unavailable(message)
    }

    pub fn newer_schema_error(found: i64) -> SemanticVectorStoreError {
        SemanticVectorStoreError::newer_schema(found)
    }

    pub fn semantic_vector_schema_version() -> i64 {
        super::vector_store_schema::SEMANTIC_VECTOR_SCHEMA_VERSION
    }

    pub fn seed_filter_unaware_derived_state(root: &std::path::Path) -> Result<()> {
        super::vector_store::seed_filter_unaware_derived_state(root)
    }
}

#[cfg(test)]
fn committed_generation_recovery_error(
    recovery: ctx_history_index::CommittedPredecessorMigrationRecovery,
) -> ctx_history_index::IndexError {
    ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
        generation_id: recovery.generation_id().to_owned(),
        stage: "predecessor migration recovery",
        detail: recovery.detail().to_owned(),
    }
}

#[cfg(test)]
mod tests;
