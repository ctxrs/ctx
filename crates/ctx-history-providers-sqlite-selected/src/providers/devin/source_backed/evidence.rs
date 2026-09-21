//! Logical evidence digests for Devin sessions and planned nodes.

use sha2::{Digest, Sha256};

use crate::fingerprint::{hash_optional_text, hash_text};

use super::super::{
    chain::{DevinNodeFacts, DevinSessionPlan, DevinSpliceKind, DevinSubagentHead},
    stream::{
        DevinAmbiguousJsonNode, DevinMalformedNode, DevinMalformedSubagentHead, DevinNodeRow,
        DevinSessionRow,
    },
};

#[allow(clippy::too_many_arguments)]
pub(super) fn session_evidence(
    session: &DevinSessionRow,
    plan: &DevinSessionPlan,
    facts: &std::collections::BTreeMap<i64, DevinNodeFacts>,
    heads: &[DevinSubagentHead],
    total_rows: u64,
    overflowed: bool,
    malformed_nodes: &[DevinMalformedNode],
    malformed_heads: &[DevinMalformedSubagentHead],
    ambiguous_json_nodes: &[DevinAmbiguousJsonNode],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    hash_text(&mut digest, &session.id);
    hash_text(&mut digest, &session.working_directory);
    hash_optional_text(&mut digest, session.title.as_deref());
    hash_optional_text(&mut digest, session.model.as_deref());
    hash_optional_text(&mut digest, session.agent_mode.as_deref());
    digest.update(session.main_chain_id.unwrap_or(-1).to_be_bytes());
    digest.update([u8::from(session.malformed_main_chain_id)]);
    digest.update([
        u8::from(session.malformed_working_directory),
        u8::from(session.malformed_title),
        u8::from(session.malformed_model),
        u8::from(session.malformed_agent_mode),
        u8::from(session.malformed_hidden),
    ]);
    digest.update(session.created_at_ms.unwrap_or(-1).to_be_bytes());
    digest.update(session.last_activity_at_ms.unwrap_or(-1).to_be_bytes());
    digest.update([u8::from(session.hidden)]);
    digest.update([plan.rejection.is_some() as u8]);
    digest.update(total_rows.to_be_bytes());
    digest.update([u8::from(overflowed)]);
    digest.update(plan.imported_nodes().to_be_bytes());
    digest.update(plan.counts.ignored_nodes.to_be_bytes());
    digest.update(plan.counts.rejected_lineages.to_be_bytes());
    digest.update(plan.counts.rejected_splices.to_be_bytes());
    digest.update(u64::try_from(facts.len()).unwrap_or(u64::MAX).to_be_bytes());
    for (node_id, fact) in facts {
        digest.update(node_id.to_be_bytes());
        digest.update(fact.parent_node_id.unwrap_or(-1).to_be_bytes());
        digest.update(fact.summarized_from.unwrap_or(-1).to_be_bytes());
        digest.update(fact.subagent_chain_node_id.unwrap_or(-1).to_be_bytes());
        hash_optional_text(&mut digest, fact.subagent_agent_id.as_deref());
    }
    digest.update(u64::try_from(heads.len()).unwrap_or(u64::MAX).to_be_bytes());
    for head in heads {
        hash_text(&mut digest, &head.agent_id);
        digest.update(head.chain_node_id.to_be_bytes());
        digest.update(head.updated_at.to_be_bytes());
    }
    digest.update(
        u64::try_from(malformed_nodes.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for node in malformed_nodes {
        digest.update([u8::from(node.node_id.is_some())]);
        digest.update(node.node_id.unwrap_or_default().to_be_bytes());
        digest.update([
            u8::from(node.malformed_node_id),
            u8::from(node.malformed_parent_node_id),
            u8::from(node.malformed_chat_message),
            u8::from(node.malformed_created_at),
            u8::from(node.malformed_metadata),
            u8::from(node.oversized_payload),
        ]);
    }
    digest.update(
        u64::try_from(malformed_heads.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for head in malformed_heads {
        hash_optional_text(&mut digest, head.agent_id.as_deref());
        digest.update([u8::from(head.malformed_agent_id)]);
        digest.update([u8::from(head.malformed_chain_node_id)]);
        digest.update([u8::from(head.malformed_updated_at)]);
        digest.update([u8::from(head.exceeded_bound)]);
    }
    digest.update(
        u64::try_from(ambiguous_json_nodes.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for node in ambiguous_json_nodes {
        digest.update(node.node_id.to_be_bytes());
        digest.update([u8::from(node.metadata)]);
        digest.update([u8::from(node.chat_message)]);
    }
    digest.finalize().into()
}

pub(super) fn malformed_session_evidence(session: &DevinSessionRow) -> [u8; 32] {
    let mut digest = Sha256::new();
    hash_text(&mut digest, &session.id);
    digest.update([u8::from(session.malformed_id)]);
    hash_text(&mut digest, &session.id_storage_class);
    digest.update(session.id_bytes.to_be_bytes());
    hash_text(&mut digest, &session.working_directory);
    hash_optional_text(&mut digest, session.title.as_deref());
    hash_optional_text(&mut digest, session.model.as_deref());
    hash_optional_text(&mut digest, session.agent_mode.as_deref());
    digest.update([u8::from(session.malformed_main_chain_id)]);
    digest.update([
        u8::from(session.malformed_working_directory),
        u8::from(session.malformed_title),
        u8::from(session.malformed_model),
        u8::from(session.malformed_agent_mode),
        u8::from(session.malformed_hidden),
    ]);
    digest.update(session.created_at_ms.unwrap_or(-1).to_be_bytes());
    digest.update(session.last_activity_at_ms.unwrap_or(-1).to_be_bytes());
    digest.update([u8::from(session.hidden)]);
    digest.finalize().into()
}

pub(super) fn node_evidence(
    node: &DevinNodeRow,
    lineage_ord: u32,
    chain_ord: u32,
    splice_kind: DevinSpliceKind,
    row_digest: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(lineage_ord.to_be_bytes());
    digest.update(chain_ord.to_be_bytes());
    digest.update(node.node_id.to_be_bytes());
    digest.update(node.parent_node_id.unwrap_or(-1).to_be_bytes());
    digest.update([splice_kind.code()]);
    digest.update(row_digest);
    digest.finalize().into()
}
