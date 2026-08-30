use ctx_semantic_model::SemanticModelContract;
use sha2::{Digest, Sha256};

use super::{vector_store::SemanticChunkDocument, SemanticEventDocument};

const SEMANTIC_CHUNK_TARGET_CHARS: usize = ctx_history_index::SEMANTIC_CHUNK_TARGET_CHARS;
const SEMANTIC_CHUNK_OVERLAP_CHARS: usize = ctx_history_index::SEMANTIC_CHUNK_OVERLAP_CHARS;
const SEMANTIC_SOURCE_MAX_CHARS: usize = ctx_history_index::SEMANTIC_SOURCE_MAX_CHARS;
// Metadata is repeated for every body chunk. Limit it to one chunk so a valid
// max-facts Core record cannot amplify its header without bound.
const SEMANTIC_DOCUMENT_HEADER_MAX_CHARS: usize = SEMANTIC_CHUNK_TARGET_CHARS;

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
                text: semantic_document_input_text(doc, &text),
                start_char,
                end_char,
            },
        )
        .collect()
}

pub(super) fn semantic_document_hash(
    model_contract: &SemanticModelContract,
    doc: &SemanticEventDocument,
    source_text: &str,
    semantic_policy_fingerprint: &str,
) -> String {
    // Sequence is event authority, not embedding input. Flat catalog mutations
    // carry it separately so a Core reorder updates exact-result metadata
    // without invalidating otherwise identical vectors.
    semantic_text_hash(&format!(
        "semantic_policy: {semantic_policy_fingerprint}\n\n{}",
        semantic_embedded_document_text(model_contract, doc, source_text)
    ))
}

pub(super) fn semantic_embedded_document_text(
    model_contract: &SemanticModelContract,
    doc: &SemanticEventDocument,
    body: &str,
) -> String {
    semantic_embedded_chunk_text(model_contract, doc, body)
}

pub(super) fn semantic_embedded_chunk_text(
    model_contract: &SemanticModelContract,
    doc: &SemanticEventDocument,
    body: &str,
) -> String {
    model_contract.document_text(&semantic_document_input_text(doc, body))
}

fn semantic_document_input_text(doc: &SemanticEventDocument, body: &str) -> String {
    let header = semantic_document_header(doc);
    if header.is_empty() {
        body.to_owned()
    } else {
        format!("{header}\n\n{body}")
    }
}

pub(super) fn semantic_document_header(doc: &SemanticEventDocument) -> String {
    let mut lines = vec![
        "semantic_document: v3".to_owned(),
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
    if let Some(agent_scope) = doc.agent_scope {
        lines.push(format!("agent_scope: {}", agent_scope.as_str()));
    }
    let mut header = lines.join("\n");
    let mut header_chars = header.chars().count();
    if header_chars > SEMANTIC_DOCUMENT_HEADER_MAX_CHARS {
        return header
            .chars()
            .take(SEMANTIC_DOCUMENT_HEADER_MAX_CHARS)
            .collect();
    }
    for fact in &doc.literal_facts {
        let line = format!(
            "fact_{}: {}",
            fact.kind.as_str(),
            semantic_header_value(&fact.value, 240)
        );
        let added_chars = line.chars().count().saturating_add(1);
        if header_chars.saturating_add(added_chars) > SEMANTIC_DOCUMENT_HEADER_MAX_CHARS {
            break;
        }
        header.push('\n');
        header.push_str(&line);
        header_chars = header_chars.saturating_add(added_chars);
    }
    header
}

pub(super) fn semantic_header_value(value: &str, max_chars: usize) -> String {
    let sanitized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut output = sanitized.chars().take(max_chars).collect::<String>();
    if sanitized.chars().count() > max_chars {
        output.push_str("...");
    }
    output
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
    use ctx_history_core::{
        AgentScope, CaptureProvider, EventRole, EventType, LiteralFactKind, ProviderDeclaredFact,
        MAX_PROVIDER_DECLARED_FACTS,
    };
    use ctx_semantic_model::semantic_model_contract;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn document_hash_uses_the_contract_document_prefix_exactly_once() {
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
            agent_scope: None,
            literal_facts: Vec::new(),
            text: "daemon failed to restart".to_owned(),
        };
        let model_contract = semantic_model_contract();
        let embedded = semantic_embedded_document_text(model_contract, &document, &document.text);
        let chunks = semantic_chunks_for_document(&document, &document.text, &"0".repeat(64));

        assert_eq!(
            embedded,
            "passage: semantic_document: v3\nevent_type: message\nrole: user\n\ndaemon failed to restart"
        );
        assert_eq!(embedded.matches("passage: ").count(), 1);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text.matches("passage: ").count(), 0);
        assert_eq!(
            model_contract.document_text(&chunks[0].text),
            embedded,
            "the executor must perform the sole preparation step"
        );
        assert_eq!(
            semantic_document_hash(
                model_contract,
                &document,
                &document.text,
                "semantic-policy-fixture",
            ),
            "759a8ad7af9c74ee56fe04157b610ad76537e48c83d224bc794f95e9f14f83bc"
        );
    }

    #[test]
    fn ordinary_document_header_preserves_all_metadata_and_fact_order() {
        let document = SemanticEventDocument {
            event_id: Uuid::nil(),
            session_id: None,
            seq: 1,
            occurred_at_ms: 0,
            event_type: EventType::ToolCall,
            role: Some(EventRole::Assistant),
            rank_bucket: " tool   activity ".to_owned(),
            provider: Some(CaptureProvider::Codex),
            source_format: Some("codex  jsonl".to_owned()),
            agent_scope: Some(AgentScope::Primary),
            literal_facts: vec![
                ProviderDeclaredFact {
                    kind: LiteralFactKind::Workspace,
                    value: "/workspace/ctx".to_owned(),
                },
                ProviderDeclaredFact {
                    kind: LiteralFactKind::File,
                    value: "crates/ctx-semantic-index/src/indexing.rs".to_owned(),
                },
            ],
            text: "bound semantic metadata".to_owned(),
        };
        let expected_header = "semantic_document: v3\nevent_type: tool_call\nrole: assistant\nrank_bucket: tool activity\nprovider: codex\nsource_format: codex jsonl\nagent_scope: primary\nfact_workspace: /workspace/ctx\nfact_file: crates/ctx-semantic-index/src/indexing.rs";

        assert_eq!(semantic_document_header(&document), expected_header);
        assert_eq!(
            semantic_document_input_text(&document, &document.text),
            format!("{expected_header}\n\nbound semantic metadata")
        );
    }

    #[test]
    fn max_fact_document_bounds_the_header_repeated_across_chunks() {
        let literal_facts = (0..MAX_PROVIDER_DECLARED_FACTS)
            .map(|index| ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: format!("/hostile/fact-{index:04}-{}", "x".repeat(240)),
            })
            .collect();
        let document = SemanticEventDocument {
            event_id: Uuid::nil(),
            session_id: None,
            seq: 1,
            occurred_at_ms: 0,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            rank_bucket: "lite_turn".to_owned(),
            provider: Some(CaptureProvider::Custom),
            source_format: Some("hostile-max-facts-v1".to_owned()),
            agent_scope: Some(AgentScope::Subagent),
            literal_facts,
            text: "x".repeat(SEMANTIC_SOURCE_MAX_CHARS),
        };

        let header = semantic_document_header(&document);
        let header_chars = header.chars().count();
        assert!(header_chars <= SEMANTIC_DOCUMENT_HEADER_MAX_CHARS);
        assert!(header.contains("fact_file: /hostile/fact-0000-"));
        assert!(!header.contains(&format!(
            "fact_file: /hostile/fact-{:04}-",
            MAX_PROVIDER_DECLARED_FACTS - 1
        )));

        let chunks = semantic_chunks_for_document(&document, &document.text, &"0".repeat(64));
        let expected_chunk_prefix = format!("{header}\n\n");
        assert!(chunks.len() > 1);
        assert_eq!(
            chunks.last().map(|chunk| chunk.end_char),
            Some(SEMANTIC_SOURCE_MAX_CHARS)
        );
        for chunk in chunks {
            assert!(chunk.text.starts_with(&expected_chunk_prefix));
            assert!(chunk.text.chars().count() <= header_chars + 2 + SEMANTIC_CHUNK_TARGET_CHARS);
        }
    }
}
