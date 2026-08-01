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
    pub(in crate::semantic) record_title: Option<String>,
    pub(in crate::semantic) record_kind: Option<String>,
    pub(in crate::semantic) record_workspace: Option<String>,
    pub(in crate::semantic) text: String,
}
