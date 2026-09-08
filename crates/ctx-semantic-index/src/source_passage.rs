use anyhow::{anyhow, Result};
use ctx_history_index::{
    policy::semantic_generation_policy, CoreEventPageBudget, RankedEventRef, SemanticPassageMember,
    SemanticPassageSource, SemanticSearchEvidence, VerifiedIndex,
};

use crate::{
    indexing::{semantic_document_hash, semantic_source_text},
    SemanticModelContract, SemanticNotReady, SemanticQueryPin, SourceBackedSemanticDocumentBuilder,
};

impl SemanticQueryPin {
    /// Reconstructs only a selected result's source through the indexing owner.
    /// The caller retains its original query contract and exact Core pin.
    pub fn resolve_passage(
        &self,
        index: &VerifiedIndex,
        contract: &SemanticModelContract,
        expected: &RankedEventRef,
        evidence: &SemanticSearchEvidence,
    ) -> Result<SemanticPassageSource> {
        self.requires_embedding(index)?;
        let mismatch = || {
            anyhow::Error::new(SemanticNotReady::new(
                "semantic_projection_event_mismatch",
                "winning semantic passage does not match its pinned Core source",
            ))
        };
        if evidence.core_generation_id != index.generation_id() {
            return Err(mismatch());
        }
        let ids = [expected.event_id];
        let record = index
            .stream_core_events_by_ids_with_strict_per_record_budget(
                &ids,
                1,
                CoreEventPageBudget::new(
                    ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES,
                    ctx_history_core::MAX_CORE_CONTENT_BYTES,
                ),
            )?
            .ok_or_else(mismatch)?
            .next()
            .transpose()?
            .ok_or_else(mismatch)?;
        if RankedEventRef::from(&record.event) != *expected {
            return Err(mismatch());
        }
        let (document, members) = SourceBackedSemanticDocumentBuilder::new(index)
            .build_source(&record)?
            .ok_or_else(mismatch)?;
        let source = semantic_source_text(&document.text);
        let policy = semantic_generation_policy(contract).canonical_sha256()?;
        if semantic_document_hash(contract, &document, &source, &policy)
            != evidence.source_text_hash
        {
            return Err(mismatch());
        }
        let offsets = source
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(source.len()))
            .collect::<Vec<_>>();
        if evidence.start_char >= evidence.end_char || evidence.end_char >= offsets.len() {
            return Err(mismatch());
        }
        let start_byte = offsets[evidence.start_char];
        let end_byte = offsets[evidence.end_char];
        let members = members
            .into_iter()
            .filter_map(|member| {
                let start = member.source_range.start;
                let end = member.source_range.end.min(offsets.len() - 1);
                (start < end).then(|| SemanticPassageMember {
                    event: member.event,
                    byte_range: offsets[start]..offsets[end],
                    content_start_char: member.content_start_char,
                })
            })
            .collect::<Vec<_>>();
        if members.is_empty() {
            return Err(anyhow!(
                "winning semantic source has no Core content member"
            ));
        }
        let truncated =
            source.len() < document.text.len() || start_byte > 0 || end_byte < source.len();
        Ok(SemanticPassageSource {
            // Keep the complete reconstructed source until presentation has
            // checked whether a grapheme crosses the scalar indexing cap.
            text: document.text,
            byte_range: start_byte..end_byte,
            truncated,
            members,
        })
    }
}
