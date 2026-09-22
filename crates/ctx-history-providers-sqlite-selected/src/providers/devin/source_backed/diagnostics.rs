//! Bounded, record-local diagnostics for Devin source-backed capture.

use std::collections::{BTreeMap, BTreeSet};

use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDraft,
    SourceBackedRecordRejectionDrafts,
};
use ctx_history_core::{CaptureProvider, SourceKey};

use super::super::{
    chain::{DevinNodeFacts, DevinSessionRejection},
    stream::{
        DevinAmbiguousJsonNode, DevinMalformedNode, DevinMalformedParentNode,
        DevinMalformedSubagentHead, DevinSessionRow,
    },
    tool_state::DevinToolState,
};

pub(super) fn diagnostic_id(value: &str) -> String {
    value.chars().take(128).collect()
}

pub(super) fn malformed_parent_traversed(
    facts: &BTreeMap<i64, DevinNodeFacts>,
    main_chain_id: Option<i64>,
    malformed_parent_nodes: &[DevinMalformedParentNode],
) -> Option<i64> {
    let malformed = malformed_parent_nodes
        .iter()
        .map(|node| node.node_id)
        .collect::<BTreeSet<_>>();
    let mut cursor = main_chain_id?;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(cursor) {
            return None;
        }
        if malformed.contains(&cursor) {
            return Some(cursor);
        }
        let node = facts.get(&cursor)?;
        let parent = node.parent_node_id?;
        if malformed.contains(&parent) {
            return Some(parent);
        }
        cursor = parent;
    }
}

pub(super) fn session_rejection_detail(reason: &DevinSessionRejection) -> &'static str {
    match reason {
        DevinSessionRejection::MissingChainAnchor => "main_chain_id names an absent node",
        DevinSessionRejection::BrokenChainParent => "a retained chain parent is absent",
        DevinSessionRejection::NonMonotonicParent => {
            "a retained chain parent does not precede its child"
        }
        DevinSessionRejection::TooManyNodes => "the message forest exceeds the planning bound",
    }
}

pub(super) fn record_devin_rejection(
    rejections: &mut SourceBackedRecordRejectionDrafts,
    source: &SourceKey,
    source_selector: &str,
    node_id: Option<i64>,
    class: SourceBackedRecordRejectionClass,
    detail: String,
) {
    rejections.record(SourceBackedRecordRejectionDraft {
        source: source.clone(),
        provider: CaptureProvider::Devin,
        source_selector: source_selector.to_owned(),
        line_number: node_id
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(0),
        payload_type: Some("sqlite_row".to_owned()),
        class,
        detail,
    });
}

pub(super) fn record_malformed_head_rejection(
    rejections: &mut SourceBackedRecordRejectionDrafts,
    source: &SourceKey,
    source_selector: &str,
    session: &DevinSessionRow,
    malformed: &DevinMalformedSubagentHead,
) {
    let mut fields = Vec::with_capacity(3);
    if malformed.malformed_agent_id {
        fields.push("agent_id");
    }
    if malformed.malformed_chain_node_id {
        fields.push("chain_node_id");
    }
    if malformed.malformed_updated_at {
        fields.push("updated_at");
    }
    let agent_id = malformed
        .agent_id
        .as_deref()
        .map(diagnostic_id)
        .unwrap_or_else(|| "<invalid-agent-id>".to_owned());
    let detail = if malformed.exceeded_bound {
        "the provider's retained durable-head bound was exceeded".to_owned()
    } else {
        match fields.as_slice() {
            [field @ ("chain_node_id" | "updated_at")] => {
                format!("{field} has a non-integer SQLite scalar")
            }
            [field] => format!("{field} has an invalid SQLite scalar or text encoding"),
            _ => format!(
                "{} have invalid SQLite scalars or text encodings",
                fields.join(", ")
            ),
        }
    };
    record_devin_rejection(
        rejections,
        source,
        source_selector,
        None,
        SourceBackedRecordRejectionClass::MalformedRecord,
        format!(
            "Devin session {} durable subagent head {agent_id} was rejected: {detail}",
            diagnostic_id(&session.id),
        ),
    );
}

pub(super) fn record_malformed_session_metadata_rejection(
    rejections: &mut SourceBackedRecordRejectionDrafts,
    source: &SourceKey,
    source_selector: &str,
    session: &DevinSessionRow,
) -> bool {
    let mut fields = Vec::with_capacity(5);
    if session.malformed_working_directory {
        fields.push("working_directory");
    }
    if session.malformed_title {
        fields.push("title");
    }
    if session.malformed_model {
        fields.push("model");
    }
    if session.malformed_agent_mode {
        fields.push("agent_mode");
    }
    if session.malformed_hidden {
        fields.push("hidden");
    }
    if fields.is_empty() {
        return false;
    }
    record_devin_rejection(
        rejections,
        source,
        source_selector,
        None,
        SourceBackedRecordRejectionClass::MalformedRecord,
        format!(
            "Devin session {} ignored malformed session metadata fields: {}",
            diagnostic_id(&session.id),
            fields.join(", "),
        ),
    );
    true
}

pub(super) fn record_malformed_node_rejection(
    rejections: &mut SourceBackedRecordRejectionDrafts,
    source: &SourceKey,
    source_selector: &str,
    session: &DevinSessionRow,
    malformed: &DevinMalformedNode,
) {
    let mut fields = Vec::with_capacity(6);
    if malformed.malformed_node_id {
        fields.push("node_id");
    }
    if malformed.malformed_parent_node_id {
        fields.push("parent_node_id");
    }
    if malformed.malformed_chat_message {
        fields.push("chat_message");
    }
    if malformed.malformed_created_at {
        fields.push("created_at");
    }
    if malformed.malformed_metadata {
        fields.push("metadata");
    }
    if malformed.oversized_payload {
        fields.push("combined payload size");
    }
    let subject = malformed.node_id.map_or_else(
        || "message_nodes row".to_owned(),
        |node_id| format!("node {node_id}"),
    );
    let detail = match fields.as_slice() {
        [field @ ("node_id" | "parent_node_id" | "created_at")] => {
            format!("{field} has a non-integer SQLite scalar")
        }
        [field] => format!("{field} has an invalid SQLite scalar or text encoding"),
        _ => format!(
            "{} have invalid SQLite scalars or text encodings",
            fields.join(", ")
        ),
    };
    record_devin_rejection(
        rejections,
        source,
        source_selector,
        malformed.node_id,
        SourceBackedRecordRejectionClass::MalformedRecord,
        format!(
            "Devin session {} {subject} was rejected: {detail}",
            diagnostic_id(&session.id),
        ),
    );
}

pub(super) fn record_ambiguous_json_rejection(
    rejections: &mut SourceBackedRecordRejectionDrafts,
    source: &SourceKey,
    source_selector: &str,
    session: &DevinSessionRow,
    ambiguous: &DevinAmbiguousJsonNode,
) {
    let fields = match (ambiguous.metadata, ambiguous.chat_message) {
        (true, true) => "metadata and chat_message",
        (true, false) => "metadata",
        (false, true) => "chat_message",
        (false, false) => unreachable!("ambiguous JSON row must name an ambiguous field"),
    };
    record_devin_rejection(
        rejections,
        source,
        source_selector,
        Some(ambiguous.node_id),
        SourceBackedRecordRejectionClass::MalformedRecord,
        format!(
            "Devin session {} node {} was rejected: {fields} has ambiguous duplicate JSON keys",
            diagnostic_id(&session.id),
            ambiguous.node_id,
        ),
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_malformed_tool_state_rejections(
    rejections: &mut SourceBackedRecordRejectionDrafts,
    source: &SourceKey,
    source_selector: &str,
    session: &DevinSessionRow,
    node_id: i64,
    call_id: &str,
    state: &DevinToolState,
) {
    for (field, reason) in state.malformed_fields() {
        record_devin_rejection(
            rejections,
            source,
            source_selector,
            Some(node_id),
            SourceBackedRecordRejectionClass::MalformedRecord,
            format!(
                "Devin session {} node {node_id} ignored optional tool_call_state enrichment for call {}: {field} {reason}",
                diagnostic_id(&session.id),
                diagnostic_id(call_id),
            ),
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_repeated_devin_rejection(
    rejections: &mut SourceBackedRecordRejectionDrafts,
    source: &SourceKey,
    source_selector: &str,
    node_id: Option<i64>,
    count: u64,
    class: SourceBackedRecordRejectionClass,
    detail: String,
) {
    let retained = count.min(1);
    if retained != 0 {
        record_devin_rejection(
            rejections,
            source,
            source_selector,
            node_id,
            class,
            detail,
        );
    }
    let omitted = count.saturating_sub(retained);
    rejections.record_omitted(usize::try_from(omitted).unwrap_or(usize::MAX));
}
