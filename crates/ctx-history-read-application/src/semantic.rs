use ctx_history_index_query::{CompiledSearchFilter, EventSearchCandidate, VerifiedIndex};
use serde_json::Value;
use thiserror::Error;

/// A stable, typed explanation for semantic retrieval being unavailable.
///
/// Application adapters translate these reasons to their wire-specific codes
/// and user-facing remediation. The query layer never interprets process or
/// service lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticReason {
    PolicyDisabled,
    PlatformUnsupported,
    ExecutionUnavailable,
    ContentScopeUnsupported,
    EventTypeUnsupported,
    QueryServiceUnavailable,
    StoreUnavailable,
    StoreMissing,
    GenerationUnreadable,
    GenerationNotAcknowledged,
    GenerationReceiptMismatch,
    ProjectionEventMismatch,
    Adapter(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticAvailability {
    Available,
    Unavailable(SemanticReason),
}

impl SemanticReason {
    pub fn from_adapter_code(code: &'static str) -> Self {
        match code {
            "semantic_query_service_unavailable" => Self::QueryServiceUnavailable,
            "semantic_store_unavailable" => Self::StoreUnavailable,
            "semantic_store_missing" => Self::StoreMissing,
            "semantic_generation_unreadable" => Self::GenerationUnreadable,
            "semantic_generation_not_acknowledged" => Self::GenerationNotAcknowledged,
            "semantic_generation_receipt_mismatch" => Self::GenerationReceiptMismatch,
            "semantic_projection_event_mismatch" => Self::ProjectionEventMismatch,
            other => Self::Adapter(other),
        }
    }
}

#[derive(Debug)]
pub struct HistorySemanticBatch {
    pub candidates: Vec<EventSearchCandidate>,
    pub diagnostics: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HistorySemanticError {
    #[error("semantic retrieval is not ready ({reason:?}): {detail}")]
    NotReady {
        reason: SemanticReason,
        detail: String,
        retryable: bool,
    },
    #[error("{detail}")]
    Failed { detail: String },
}

impl HistorySemanticError {
    pub fn not_ready(reason: SemanticReason, detail: impl Into<String>, retryable: bool) -> Self {
        Self::NotReady {
            reason,
            detail: detail.into(),
            retryable,
        }
    }

    pub fn failed(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
        }
    }

    pub const fn reason(&self) -> Option<SemanticReason> {
        match self {
            Self::NotReady { reason, .. } => Some(*reason),
            Self::Failed { .. } => None,
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::NotReady { detail, .. } | Self::Failed { detail } => detail,
        }
    }

    pub const fn retryable(&self) -> bool {
        match self {
            Self::NotReady { retryable, .. } => *retryable,
            Self::Failed { .. } => false,
        }
    }
}

pub trait HistorySemanticQuery {
    /// Prepare one normalized alternative in caller order.
    ///
    /// Implementations retain the resulting vector on the query session so a
    /// later [`Self::candidates`] call can score all alternatives in one exact
    /// vector traversal. Returning diagnostics here lets callers preserve the
    /// completed prefix when a later alternative fails.
    fn prepare_alternative(&mut self, query: &str) -> Result<Value, HistorySemanticError>;

    /// Retrieve one globally ranked candidate pool for every prepared
    /// alternative.
    fn candidates(
        &mut self,
        filter: &CompiledSearchFilter,
        candidate_limit: usize,
    ) -> Result<HistorySemanticBatch, HistorySemanticError>;
}

pub trait HistorySemanticPort: Send + Sync {
    type Query<'a>: HistorySemanticQuery + 'a
    where
        Self: 'a;

    fn begin_query<'a>(
        &'a self,
        index: &'a VerifiedIndex,
    ) -> Result<Self::Query<'a>, HistorySemanticError>;
}

#[cfg(test)]
mod tests;
