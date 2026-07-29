use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType};
use uuid::Uuid;

/// Exact source-backed event content prepared for semantic indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::semantic) struct SemanticEventDocument {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) history_record_id: Option<Uuid>,
    pub(in crate::semantic) session_id: Option<Uuid>,
    pub(in crate::semantic) seq: u64,
    /// Activity time used to order semantic documents, including a paired assistant reply.
    pub(in crate::semantic) occurred_at_ms: i64,
    /// User-anchor time used with `seq` to advance a legacy Store pagination frontier.
    pub(in crate::semantic) anchor_occurred_at_ms: i64,
    pub(in crate::semantic) event_type: EventType,
    pub(in crate::semantic) role: Option<EventRole>,
    pub(in crate::semantic) rank_bucket: String,
    pub(in crate::semantic) provider: Option<CaptureProvider>,
    pub(in crate::semantic) source_format: Option<String>,
    pub(in crate::semantic) agent_type: Option<AgentType>,
    pub(in crate::semantic) session_is_primary: Option<bool>,
    pub(in crate::semantic) cwd: Option<String>,
    pub(in crate::semantic) raw_source_path: Option<String>,
    pub(in crate::semantic) record_title: Option<String>,
    pub(in crate::semantic) record_kind: Option<String>,
    pub(in crate::semantic) record_workspace: Option<String>,
    pub(in crate::semantic) text: String,
}

// The legacy Store projection remains compiled until its SQL module is removed.
// Keep that compatibility at call sites without making its DTO part of the CLI
// semantic type contract.
macro_rules! semantic_event_document_from_store_projection {
    ($document:expr) => {{
        let document = $document;
        $crate::semantic::SemanticEventDocument {
            event_id: document.event_id,
            history_record_id: document.history_record_id,
            session_id: document.session_id,
            seq: document.seq,
            occurred_at_ms: document.occurred_at_ms,
            anchor_occurred_at_ms: document.anchor_occurred_at_ms,
            event_type: document.event_type,
            role: document.role,
            rank_bucket: document.rank_bucket,
            provider: document.provider,
            source_format: document.source_format,
            agent_type: document.agent_type,
            session_is_primary: document.session_is_primary,
            cwd: document.cwd,
            raw_source_path: document.raw_source_path,
            record_title: document.record_title,
            record_kind: document.record_kind,
            record_workspace: document.record_workspace,
            text: document.text,
        }
    }};
}

pub(in crate::semantic) use semantic_event_document_from_store_projection;
