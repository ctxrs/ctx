use ctx_history_core::{AgentScope, CaptureProvider, EventRole, EventType, ProviderDeclaredFact};
use uuid::Uuid;

/// Exact source-backed event content prepared for semantic indexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticEventDocument {
    pub(crate) event_id: Uuid,
    pub(crate) session_id: Option<Uuid>,
    pub(crate) seq: u64,
    /// Activity time used to order semantic documents, including paired assistant replies.
    pub(crate) occurred_at_ms: i64,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) rank_bucket: String,
    pub(crate) provider: Option<CaptureProvider>,
    pub(crate) source_format: Option<String>,
    pub(crate) agent_scope: Option<AgentScope>,
    pub(crate) literal_facts: Vec<ProviderDeclaredFact>,
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
        agent_scope: Option<AgentScope>,
        literal_facts: Vec<ProviderDeclaredFact>,
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
            agent_scope,
            literal_facts,
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
