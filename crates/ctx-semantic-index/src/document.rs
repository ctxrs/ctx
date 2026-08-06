use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType};
use uuid::Uuid;

/// Exact source-backed event content prepared for semantic indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEventDocument {
    pub(crate) event_id: Uuid,
    pub(crate) session_id: Option<Uuid>,
    pub(crate) seq: u64,
    /// Activity time used to order semantic documents, including a paired assistant reply.
    pub(crate) occurred_at_ms: i64,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) rank_bucket: String,
    pub(crate) provider: Option<CaptureProvider>,
    pub(crate) source_format: Option<String>,
    pub(crate) agent_type: Option<AgentType>,
    pub(crate) session_is_primary: Option<bool>,
    pub(crate) cwd: Option<String>,
    pub(crate) record_title: Option<String>,
    pub(crate) record_kind: Option<String>,
    pub(crate) record_workspace: Option<String>,
    pub(crate) text: String,
}

impl SemanticEventDocument {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_id: Uuid,
        session_id: Option<Uuid>,
        seq: u64,
        occurred_at_ms: i64,
        event_type: EventType,
        role: Option<EventRole>,
        rank_bucket: String,
        provider: Option<CaptureProvider>,
        source_format: Option<String>,
        agent_type: Option<AgentType>,
        session_is_primary: Option<bool>,
        cwd: Option<String>,
        record_title: Option<String>,
        record_kind: Option<String>,
        record_workspace: Option<String>,
        text: String,
    ) -> Self {
        Self {
            event_id,
            session_id,
            seq,
            occurred_at_ms,
            event_type,
            role,
            rank_bucket,
            provider,
            source_format,
            agent_type,
            session_is_primary,
            cwd,
            record_title,
            record_kind,
            record_workspace,
            text,
        }
    }

    pub fn event_id(&self) -> Uuid {
        self.event_id
    }

    pub fn session_id(&self) -> Option<Uuid> {
        self.session_id
    }

    pub fn sequence(&self) -> u64 {
        self.seq
    }

    pub fn occurred_at_ms(&self) -> i64 {
        self.occurred_at_ms
    }

    pub fn provider(&self) -> Option<CaptureProvider> {
        self.provider
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}
