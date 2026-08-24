use std::{fmt, path::PathBuf};

use ctx_history_core::CaptureProvider;
use serde_json::Value;
use uuid::Uuid;

/// One transport-neutral application operation exposed through MCP.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOperation {
    Status,
    Sources,
    Search(ToolSearchRequest),
    ShowSession(ShowSessionRequest),
    ShowEvent(ShowEventRequest),
    QueryEvents(QueryEventsRequest),
}

impl ToolOperation {
    pub fn invocation_usage(&self) -> ToolUsageFacts {
        ToolUsageFacts::default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSearchRequest {
    pub query: String,
    pub limit: usize,
    pub provider: Option<CaptureProvider>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub source_roots: Vec<String>,
    pub source_groups: Vec<String>,
    pub workspace: Option<String>,
    pub since: Option<String>,
    pub primary_only: bool,
    pub content_scope: ToolSearchContentScope,
    pub event_type: Option<String>,
    pub file: Option<PathBuf>,
    pub session: Option<String>,
    pub events: bool,
    pub include_current_session: bool,
    pub backend: Option<ToolSearchBackend>,
    pub semantic_weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSearchBackend {
    Lexical,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolSearchContentScope {
    #[default]
    All,
    Transcript,
    Calls,
    Outputs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowSessionRequest {
    pub selector: String,
    pub mode: ToolTranscriptMode,
    pub limit: usize,
    pub cursor: Option<String>,
    pub output_limit_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolTranscriptMode {
    Full,
    Lite,
    Log,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowEventRequest {
    pub selector: String,
    pub before: usize,
    pub after: usize,
    pub window: Option<usize>,
    pub output_limit_bytes: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryEventsRequest {
    pub since: Option<String>,
    pub until: Option<String>,
    pub filters: QueryEventFilters,
    pub cursor: Option<String>,
    pub content: ToolEventContent,
    pub limit: u64,
    pub output_limit_bytes: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryEventFilters {
    pub providers: Vec<String>,
    pub source_identity: Option<Uuid>,
    pub history_source: Option<String>,
    pub provider_key: Option<String>,
    pub source_id: Option<String>,
    pub source_format: Option<String>,
    pub provider_session_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub parent_session_id: Option<Uuid>,
    pub root_session_id: Option<Uuid>,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub event_type: Option<String>,
    pub role: Option<String>,
    pub agent_type: Option<String>,
    pub scope: ToolEventRangeScope,
    pub file: Option<String>,
    pub direction: ToolEventRangeDirection,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolEventRangeScope {
    #[default]
    All,
    Primary,
    Subagent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolEventRangeDirection {
    #[default]
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEventContent {
    Full,
    Text,
    None,
}

#[derive(Debug)]
pub struct ToolOutcome {
    pub structured: Value,
    /// Optional projection used only by the MCP adapter to render compact text.
    pub compact: Option<Value>,
    pub usage: ToolUsageFacts,
}

impl ToolOutcome {
    pub fn plain(structured: Value) -> Self {
        Self {
            structured,
            compact: None,
            usage: ToolUsageFacts::default(),
        }
    }

    pub fn with_compact(structured: Value, compact: Value) -> Self {
        Self {
            structured,
            compact: Some(compact),
            usage: ToolUsageFacts::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolUsageFacts {
    pub search: Option<ToolSearchUsageFacts>,
}

impl ToolUsageFacts {
    pub fn merge(&mut self, additional: Self) {
        if additional.search.is_some() {
            self.search = additional.search;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSearchUsageFacts {
    pub context_complete: bool,
    pub delivered_context_bytes: u64,
    pub matched_normalized_session_bytes: u64,
}

impl ToolSearchUsageFacts {
    pub const fn complete(
        delivered_context_bytes: u64,
        matched_normalized_session_bytes: u64,
    ) -> Self {
        Self {
            context_complete: true,
            delivered_context_bytes,
            matched_normalized_session_bytes,
        }
    }

    pub const fn unavailable() -> Self {
        Self {
            context_complete: false,
            delivered_context_bytes: 0,
            matched_normalized_session_bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpaqueMcpProxyError {
    CompanionUnavailable,
    CompanionIncompatible,
}

/// Product execution boundary. Implementations remain in the owning application.
pub trait ToolBackend: Send + Sync {
    fn execute(&self, operation: ToolOperation) -> Result<ToolOutcome, ToolExecutionError>;

    /// Proxies one already-framed companion-owned MCP request without parsing its arguments.
    fn proxy_companion_mcp(&self, request: &[u8]) -> Result<Vec<u8>, OpaqueMcpProxyError>;

    /// Resolves an MCP provider spelling through the application's provider registry.
    fn parse_provider(&self, value: &str) -> Option<CaptureProvider>;

    /// Returns the exact sorted provider names advertised in the MCP schema.
    fn provider_names(&self) -> Vec<&'static str>;
}

#[derive(Debug)]
pub struct ToolExecutionError {
    pub error: ToolBackendError,
    pub usage: ToolUsageFacts,
}

impl From<ToolBackendError> for ToolExecutionError {
    fn from(error: ToolBackendError) -> Self {
        Self {
            error,
            usage: ToolUsageFacts::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructuredToolError {
    pub structured: Value,
    pub detail: String,
}

#[derive(Debug)]
pub enum ToolBackendError {
    InvalidRequest {
        detail: String,
    },
    EventQuery(StructuredToolError),
    SourceUnavailable,
    GenerationChanged,
    GenerationAuthority(StructuredToolError),
    Cursor {
        kind: CursorFailureKind,
        detail: String,
    },
    OutputLimit {
        event_id: Uuid,
        actual_bytes: usize,
        maximum_bytes: usize,
    },
    SemanticNotReady {
        code: &'static str,
        detail: String,
        retryable: bool,
    },
    Internal {
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorFailureKind {
    Stale,
    Mismatch,
    Invalid,
}

impl ToolBackendError {
    pub fn invalid_request(detail: impl Into<String>) -> Self {
        Self::InvalidRequest {
            detail: detail.into(),
        }
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ToolBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest { detail }
            | Self::Cursor { detail, .. }
            | Self::Internal { detail } => formatter.write_str(detail),
            Self::EventQuery(error) | Self::GenerationAuthority(error) => {
                formatter.write_str(&error.detail)
            }
            Self::SourceUnavailable => formatter.write_str("source_unavailable"),
            Self::GenerationChanged => formatter.write_str(
                "History changed while ctx was opening the searchable generation. Retry the same request.",
            ),
            Self::OutputLimit {
                event_id,
                actual_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "Core content output for ctx event {event_id} requires {actual_bytes} bytes; the presentation limit is {maximum_bytes} bytes"
            ),
            Self::SemanticNotReady { code, detail, .. } => write!(
                formatter,
                "source-backed semantic search is not ready ({code}): {detail}"
            ),
        }
    }
}

impl std::error::Error for ToolBackendError {}
