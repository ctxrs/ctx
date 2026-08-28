use std::fs;

use anyhow::Result;
use ctx_semantic_model::{semantic_model_contract, SemanticModelContract};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    source_backed_semantic_vector_path, vector_store::SemanticVectorSearch, SemanticChunkDocument,
    SemanticVectorStore,
};

fn test_contract() -> SemanticModelContract {
    semantic_model_contract().clone()
}

fn test_embedding(contract: &SemanticModelContract, first: f32, second: f32) -> Vec<f32> {
    let mut embedding = vec![0.0; contract.dimensions()];
    let norm = first.mul_add(first, second * second).sqrt();
    if norm > 0.0 {
        embedding[0] = first / norm;
        embedding[1] = second / norm;
    }
    embedding
}

fn test_chunk(event_id: Uuid, seq: u64, source_hash: &str) -> SemanticChunkDocument {
    test_chunk_at(event_id, seq, source_hash, 0, 1)
}

fn test_chunk_at(
    event_id: Uuid,
    seq: u64,
    source_hash: &str,
    chunk_index: usize,
    _chunk_count: usize,
) -> SemanticChunkDocument {
    let source_text_hash = if source_hash.len() == 64
        && source_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        source_hash.to_owned()
    } else {
        format!("{:x}", Sha256::digest(source_hash.as_bytes()))
    };
    SemanticChunkDocument {
        event_id,
        seq,
        chunk_index,
        source_text_hash,
        text: String::new(),
        start_char: chunk_index.saturating_mul(10),
        end_char: chunk_index.saturating_mul(10).saturating_add(12),
    }
}

mod vector_store;
