use super::*;

pub trait SemanticBatchEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>>;
}

pub(super) struct SourceProjectionWorkers<'a> {
    pub(super) builder: &'a mut dyn SemanticDocumentBuilder,
    pub(super) embedder: &'a mut dyn SemanticBatchEmbedder,
}

#[derive(Debug)]
pub(super) struct ResolvedSourceDocument {
    pub(super) event_id: StableEntityId,
    pub(super) stable_identity: Vec<u8>,
    pub(super) source_text_sha256: String,
    pub(super) seq: u64,
    pub(super) chunks: Vec<SemanticChunkDocument>,
}

pub(super) fn external_embedding_chunk_limit(
    model_contract: &SemanticModelContract,
) -> Option<usize> {
    model_contract
        .external_space()
        .map(|space| space.max_inputs_per_request())
}

pub(super) fn source_event_page_limit(model_contract: &SemanticModelContract) -> usize {
    external_embedding_chunk_limit(model_contract).unwrap_or(MAX_SOURCE_EVENT_PAGE_ITEMS)
}

pub(super) fn embed_chunks_in_bounded_batches(
    embedder: &mut dyn SemanticBatchEmbedder,
    chunks: Vec<SemanticChunkDocument>,
    dimensions: usize,
    batch_limit: Option<usize>,
) -> Result<Vec<(SemanticChunkDocument, Vec<f32>)>> {
    let batch_limit = batch_limit.unwrap_or(chunks.len()).max(1);
    let mut chunks = chunks.into_iter();
    let mut replacements = Vec::new();
    loop {
        let batch = chunks.by_ref().take(batch_limit).collect::<Vec<_>>();
        if batch.is_empty() {
            return Ok(replacements);
        }
        let embeddings = embedder.embed_chunks(&batch)?;
        if embeddings.len() != batch.len()
            || embeddings
                .iter()
                .any(|embedding| embedding.len() != dimensions)
        {
            return Err(SemanticVectorStoreError::unavailable(
                "source-backed semantic embedder returned an invalid batch",
            )
            .into());
        }
        replacements.extend(batch.into_iter().zip(embeddings));
    }
}
