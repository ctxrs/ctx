use anyhow::Result;

use super::*;
use crate::vector_store_schema::SemanticVectorStoreError;

// A derived header may be shortened to leave useful room for original text.
const BODY_RESERVE_CHARS: usize = 256;
const MAX_DOCUMENT_CHUNKS: usize = 1_024;
const FIT_REFINEMENT_STEPS: usize = 2;
// Includes the original window, header reservation, shrinking and refinement.
const MAX_WINDOW_ASSESSMENTS: usize = 28;

/// Finalize source spans only after the selected executor accepts the complete
/// header/body input. Each candidate is tested; token-count monotonicity is not
/// assumed and this bounded refinement does not promise the largest fit.
pub(crate) fn semantic_chunks_for_document_with_fit(
    doc: &SemanticEventDocument,
    source_text: &str,
    source_text_hash: &str,
    fits: &mut dyn FnMut(&str) -> Result<bool>,
) -> Result<Vec<SemanticChunkDocument>> {
    let chars: Vec<char> = source_text
        .chars()
        .take(SEMANTIC_SOURCE_MAX_CHARS + 1)
        .collect();
    if chars.len() > SEMANTIC_SOURCE_MAX_CHARS {
        return Err(SemanticVectorStoreError::unavailable(
            "semantic source exceeds its chunking bound",
        )
        .into());
    }
    let header: Vec<char> = semantic_document_header(doc).chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chunks.len() == MAX_DOCUMENT_CHUNKS {
            return Err(SemanticVectorStoreError::unavailable(
                "semantic document exceeds the token-fit chunk limit",
            )
            .into());
        }
        let mut assessments = 0;
        let mut assess = |input: &str| {
            if assessments == MAX_WINDOW_ASSESSMENTS {
                return Err(SemanticVectorStoreError::unavailable(
                    "semantic document exhausted its bounded token-fit assessment budget",
                )
                .into());
            }
            assessments += 1;
            fits(input)
        };
        let mut end = (start + SEMANTIC_CHUNK_TARGET_CHARS).min(chars.len());
        if end < chars.len() {
            let floor = end.saturating_sub(150).max(start + 1);
            if let Some(index) = (floor..end).rev().find(|&i| chars[i].is_whitespace()) {
                end = index + 1;
            }
        }
        let mut header_len = header.len();
        let mut input = input_text(&header[..header_len], &chars[start..end]);
        if !assess(&input)? {
            // Retain the header when it can accompany a useful body prefix.
            // Dense optional facts must not permanently consume the body budget.
            let reserve_end = (start + BODY_RESERVE_CHARS).min(end);
            let reserve_fits = loop {
                let fit = assess(&input_text(
                    &header[..header_len],
                    &chars[start..reserve_end],
                ))?;
                if fit || header_len == 0 {
                    break fit;
                }
                header_len /= 2;
            };
            let mut rejected_end = end;
            loop {
                input = input_text(&header[..header_len], &chars[start..end]);
                if assess(&input)? {
                    break;
                }
                rejected_end = end;
                if end == start + 1 {
                    return Err(SemanticVectorStoreError::unavailable("semantic document has no fitting source window within the bounded token-fit plan").into());
                }
                let mut next_end = start + (end - start) / 2;
                if reserve_fits {
                    next_end = next_end.max(reserve_end);
                }
                if next_end >= end {
                    return Err(SemanticVectorStoreError::unavailable(
                        "semantic document fit changed before a source window could be finalized",
                    )
                    .into());
                }
                end = next_end;
            }
            // Recover some spare capacity without an exact-max search. Failed
            // probes only choose the next candidate; accepted spans always fit.
            for _ in 0..FIT_REFINEMENT_STEPS {
                let candidate = end + (rejected_end - end) / 2;
                if candidate <= end {
                    break;
                }
                let refined = input_text(&header[..header_len], &chars[start..candidate]);
                if assess(&refined)? {
                    end = candidate;
                    input = refined;
                } else {
                    rejected_end = candidate;
                }
            }
        }
        chunks.push(SemanticChunkDocument {
            event_id: doc.event_id,
            seq: doc.seq,
            chunk_index: chunks.len(),
            source_text_hash: source_text_hash.to_owned(),
            text: input,
            start_char: start,
            end_char: end,
        });
        if end == chars.len() {
            break;
        }
        // Retain desired overlap for normal windows; cap it for short fits so
        // every iteration advances and no original source position is skipped.
        start = end - SEMANTIC_CHUNK_OVERLAP_CHARS.min((end - start) / 2);
    }
    Ok(chunks)
}

fn input_text(header: &[char], body: &[char]) -> String {
    let mut text: String = header.iter().collect();
    if !header.is_empty() {
        text.push_str("\n\n");
    }
    text.extend(body);
    text
}

#[cfg(test)]
mod tests;
