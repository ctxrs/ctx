use std::path::Path;

use ctx_semantic_model::semantic_e5_passage_text;
use sha2::{Digest, Sha256};

use super::{vector_store::SemanticChunkDocument, SemanticEventDocument};

const SEMANTIC_CHUNK_TARGET_CHARS: usize = ctx_history_index::SEMANTIC_CHUNK_TARGET_CHARS;
const SEMANTIC_CHUNK_OVERLAP_CHARS: usize = ctx_history_index::SEMANTIC_CHUNK_OVERLAP_CHARS;
const SEMANTIC_SOURCE_MAX_CHARS: usize = ctx_history_index::SEMANTIC_SOURCE_MAX_CHARS;

pub(super) fn semantic_source_text(text: &str) -> String {
    text.chars().take(SEMANTIC_SOURCE_MAX_CHARS).collect()
}

pub(super) fn semantic_chunks_for_document(
    doc: &SemanticEventDocument,
    source_text: &str,
    source_text_hash: &str,
) -> Vec<SemanticChunkDocument> {
    let chunks = semantic_text_chunks(source_text);
    chunks
        .into_iter()
        .enumerate()
        .map(
            |(chunk_index, (start_char, end_char, text))| SemanticChunkDocument {
                event_id: doc.event_id,
                seq: doc.seq,
                chunk_index,
                source_text_hash: source_text_hash.to_owned(),
                text: semantic_embedded_chunk_text(doc, &text),
                start_char,
                end_char,
            },
        )
        .collect()
}

pub(super) fn semantic_document_hash(
    doc: &SemanticEventDocument,
    source_text: &str,
    semantic_policy_fingerprint: &str,
) -> String {
    // Sequence is event authority, not embedding input. Flat catalog mutations
    // carry it separately so a Core reorder updates exact-result metadata
    // without invalidating otherwise identical vectors.
    semantic_text_hash(&format!(
        "semantic_policy: {semantic_policy_fingerprint}\n\n{}",
        semantic_embedded_document_text(doc, source_text)
    ))
}

pub(super) fn semantic_embedded_document_text(doc: &SemanticEventDocument, body: &str) -> String {
    semantic_embedded_chunk_text(doc, body)
}

pub(super) fn semantic_embedded_chunk_text(doc: &SemanticEventDocument, body: &str) -> String {
    let header = semantic_document_header(doc);
    let text = if header.is_empty() {
        body.to_owned()
    } else {
        format!("{header}\n\n{body}")
    };
    semantic_e5_passage_text(&text)
}

pub(super) fn semantic_document_header(doc: &SemanticEventDocument) -> String {
    let mut lines = vec![
        "semantic_document: v2".to_owned(),
        format!("event_type: {}", doc.event_type.as_str()),
    ];
    if let Some(role) = doc.role {
        lines.push(format!("role: {}", role.as_str()));
    }
    if !doc.rank_bucket.trim().is_empty() {
        lines.push(format!(
            "rank_bucket: {}",
            semantic_header_value(&doc.rank_bucket, 80)
        ));
    }
    if let Some(provider) = doc.provider {
        lines.push(format!("provider: {}", provider.as_str()));
    }
    if let Some(source_format) = doc.source_format.as_deref() {
        lines.push(format!(
            "source_format: {}",
            semantic_header_value(source_format, 120)
        ));
    }
    if let Some(agent_type) = doc.agent_type {
        lines.push(format!("agent_type: {}", agent_type.as_str()));
    }
    if let Some(is_primary) = doc.session_is_primary {
        lines.push(format!(
            "session_scope: {}",
            if is_primary { "primary" } else { "subagent" }
        ));
    }
    if let Some(workspace) = doc.record_workspace.as_deref() {
        lines.push(format!(
            "workspace_hint: {}",
            semantic_header_value(workspace, 160)
        ));
    }
    if let Some(cwd) = doc.cwd.as_deref().and_then(path_basename) {
        lines.push(format!("cwd_hint: {}", semantic_header_value(cwd, 120)));
    }
    if let Some(title) = doc.record_title.as_deref() {
        lines.push(format!("title_hint: {}", semantic_header_value(title, 180)));
    }
    if let Some(kind) = doc.record_kind.as_deref() {
        lines.push(format!("record_kind: {}", semantic_header_value(kind, 80)));
    }
    lines.join("\n")
}

pub(super) fn semantic_header_value(value: &str, max_chars: usize) -> String {
    let sanitized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = sanitized.chars().take(max_chars).collect::<String>();
    if sanitized.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

pub(super) fn path_basename(path: &str) -> Option<&str> {
    Path::new(path).file_name().and_then(|value| value.to_str())
}

pub(super) fn semantic_text_chunks(text: &str) -> Vec<(usize, usize, String)> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }
    if chars.len() <= SEMANTIC_CHUNK_TARGET_CHARS {
        return vec![(0, chars.len(), text.to_owned())];
    }

    let mut chunks = Vec::new();
    let mut start = 0_usize;
    while start < chars.len() {
        let mut end = start
            .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
            .min(chars.len());
        if end < chars.len() {
            let boundary_floor = end.saturating_sub(150).max(start + 1);
            for index in (boundary_floor..end).rev() {
                if chars[index].is_whitespace() {
                    end = index + 1;
                    break;
                }
            }
        }
        if end <= start {
            end = start
                .saturating_add(SEMANTIC_CHUNK_TARGET_CHARS)
                .min(chars.len());
        }
        let chunk = chars[start..end].iter().collect::<String>();
        chunks.push((start, end, chunk));
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(SEMANTIC_CHUNK_OVERLAP_CHARS);
    }
    chunks
}

pub(super) fn semantic_text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{EventRole, EventType};
    use uuid::Uuid;

    use super::*;

    #[test]
    fn document_hash_keeps_one_e5_passage_prefix() {
        let document = SemanticEventDocument {
            event_id: Uuid::nil(),
            session_id: None,
            seq: 1,
            occurred_at_ms: 0,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            rank_bucket: String::new(),
            provider: None,
            source_format: None,
            agent_type: None,
            session_is_primary: None,
            cwd: None,
            record_title: None,
            record_kind: None,
            record_workspace: None,
            text: "daemon failed to restart".to_owned(),
        };
        let embedded = semantic_embedded_document_text(&document, &document.text);

        assert_eq!(
            embedded,
            "passage: semantic_document: v2\nevent_type: message\nrole: user\n\ndaemon failed to restart"
        );
        assert_eq!(embedded.matches("passage: ").count(), 1);
        assert_eq!(
            semantic_document_hash(&document, &document.text, "semantic-policy-fixture"),
            "a8729176eca6ebc96f9e683d5528b81c740aef0248b0e5820f1f78e03a73cedb"
        );
    }
}
