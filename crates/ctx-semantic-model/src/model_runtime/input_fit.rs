use super::*;
use crate::model_contract::SEMANTIC_MAX_SEQUENCE_LENGTH;

// Existing maximum header + body UTF-8 bytes, separators and passage prefix.
const MAX_PREPARED_DOCUMENT_BYTES: usize = 9_611;

#[derive(Debug)]
pub(super) struct SemanticDocumentInputTooLarge;

impl fmt::Display for SemanticDocumentInputTooLarge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("semantic document does not fit the selected tokenizer input limit")
    }
}
impl std::error::Error for SemanticDocumentInputTooLarge {}

impl SharedSemanticRuntime {
    pub(crate) fn document_fits(&self, prepared: &str) -> Result<bool> {
        self.lock()?
            .as_ref()
            .ok_or_else(|| anyhow!("semantic embedder was not initialized"))?
            .backend
            .document_fits(prepared)
    }
}

impl SemanticEmbeddingBackend {
    pub(super) fn embed_prepared_documents(
        &mut self,
        documents: Vec<String>,
        batch_size: usize,
    ) -> Result<Vec<Vec<f32>>> {
        let expected = documents.len();
        if expected == 0 {
            return Ok(Vec::new());
        }
        // Recheck with the backend that will actually infer, including after
        // recovery/fallback. A changed tokenizer cannot silently truncate spans.
        for document in &documents {
            if !self.document_fits(document)? {
                return Err(SemanticDocumentInputTooLarge.into());
            }
        }
        let raw = match self {
            Self::Ort { model, .. } => {
                model.embed(documents, Some(batch_size)).with_context(|| {
                    format!("embed documents with semantic model {SEMANTIC_MODEL_ID}")
                })?
            }
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.embed_documents(documents)?,
        };
        normalize_and_validate_embeddings(raw, expected)
    }

    pub(super) fn document_fits(&self, prepared: &str) -> Result<bool> {
        if prepared.len() > MAX_PREPARED_DOCUMENT_BYTES {
            return Err(SemanticDocumentInputTooLarge.into());
        }
        match self {
            Self::Ort { model, .. } => {
                if model.tokenizer.get_truncation().map(|p| p.max_length)
                    != Some(SEMANTIC_MAX_SEQUENCE_LENGTH)
                {
                    return Err(anyhow!("semantic tokenizer has an unexpected input limit"));
                }
                let encoding = model
                    .tokenizer
                    .encode(prepared, true)
                    .map_err(|error| anyhow!("assess semantic document input: {error}"))?;
                // tokenizers reserves special tokens before truncation and keeps
                // every removed sequence in overflowing. Padding adds no overflow.
                Ok(encoding.get_overflowing().is_empty())
            }
            #[cfg(target_os = "macos")]
            Self::CoreMl(model) => model.document_fits(prepared),
        }
    }
}
