use super::*;

/// Default maximum number of sessions returned by one bounded session list.
pub const DEFAULT_SESSION_LIST_LIMIT: usize = 50;

/// Absolute maximum number of sessions returned by one bounded session list.
pub const MAX_SESSION_LIST_LIMIT: usize = 1_000;

/// Typed failures for bounded session listing over one pinned generation.
#[derive(Debug, thiserror::Error)]
pub enum SessionListError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("session list limit {requested} is outside 1..={maximum}")]
    InvalidLimit { requested: usize, maximum: usize },
}

/// Filters applied when listing sessions from one immutable Core generation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionListFilters {
    pub since_unix_ms: Option<i64>,
    pub provider: Option<String>,
    pub workspace: Option<String>,
    pub no_subagents: bool,
}

/// Summary metadata for one indexed session, ordered newest activity first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummaryRecord {
    pub session_id: StableEntityId,
    pub provider: String,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub provider_session_id: Option<String>,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: Option<StableEntityId>,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub branch: Option<String>,
    pub started_at_unix_ms: Option<i64>,
    pub last_activity_at_unix_ms: Option<i64>,
    pub events: u64,
}

/// One bounded list of sessions from a single verified lexical generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListPage {
    pub generation_id: String,
    pub truncated: bool,
    pub sessions: Vec<SessionSummaryRecord>,
}
