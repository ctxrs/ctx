use ctx_history_core::{
    AgentType, Confidence, EventRole, EventType, Fidelity, FileChangeKind, SessionStatus,
    SourceKey, SourceRecordLocator, StableEntityId,
};
use thiserror::Error;
use uuid::Uuid;

use crate::StoreError;

pub const RELATIONAL_PROJECTION_SCHEMA_VERSION: u32 = 1;
pub const RELATIONAL_PROJECTION_CONTRACT_VERSION: u32 = 1;
pub const RELATIONAL_EVENT_PREVIEW_MAX_CHARS: usize = 2_048;

pub type Result<T> = std::result::Result<T, RelationalProjectionError>;

#[derive(Debug, Error)]
pub enum RelationalProjectionError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("source-backed relational projection schema is missing")]
    MissingSchema,
    #[error(
        "unsupported source-backed relational schema {schema_version}, contract {contract_version}"
    )]
    UnsupportedSchema {
        schema_version: i64,
        contract_version: i64,
    },
    #[error("source-backed relational projection is missing stable view {0}")]
    MissingStableView(String),
    #[error(
        "source-backed SQL projection is missing at {projection_path} while a committed source generation exists at {generation_path}; rebuild the relational projection from that generation"
    )]
    MissingSourceBackedSqlProjection {
        projection_path: std::path::PathBuf,
        generation_path: std::path::PathBuf,
    },
    #[error("invalid committed Core generation: {0}")]
    InvalidCoreGeneration(String),
    #[error("invalid source-backed relational record: {0}")]
    InvalidRecord(String),
    #[error("source-backed relational stream ordering violation: {0}")]
    InvalidStreamOrder(String),
    #[error(
        "source-backed relational projection expected sources {expected:?}, received {received:?}"
    )]
    SourceSetMismatch {
        expected: Vec<String>,
        received: Vec<String>,
    },
    #[error(
        "source-backed relational source {source_id} expected {expected} events, received {received}"
    )]
    SourceEventCountMismatch {
        source_id: String,
        expected: u64,
        received: u64,
    },
    #[error(
        "source-backed relational generation expected {expected} events, projected {projected}"
    )]
    GenerationEventCountMismatch { expected: u64, projected: u64 },
    #[error("source-backed relational count does not fit SQLite INTEGER: {0}")]
    CountOverflow(&'static str),
}

/// The exact post-publication receipt plus canonical generation manifest.
///
/// The integration host constructs this from `CommitReceipt` and the verified
/// index manifest. Validation occurs before any relational transaction starts.
#[derive(Debug, Clone)]
pub struct CommittedCoreGeneration {
    pub generation_id: String,
    pub manifest_json: Vec<u8>,
    pub indexed_documents: u64,
    pub certified_sources: usize,
    pub certified_source_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct RelationalSourceMetadata {
    pub source: SourceKey,
    pub source_root: Option<String>,
    pub source_path: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RelationalSessionMetadata {
    pub session_id: StableEntityId,
    pub parent_session_id: Option<StableEntityId>,
    pub root_session_id: StableEntityId,
    pub provider_session_id: Option<String>,
    pub external_agent_id: Option<String>,
    pub agent_type: AgentType,
    pub role_hint: Option<String>,
    pub is_primary: bool,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub cwd: Option<String>,
    pub source_path: Option<String>,
    pub status: SessionStatus,
    pub fidelity: Fidelity,
    pub started_at_unix_ms: Option<i64>,
    pub ended_at_unix_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct RelationalEventMetadata {
    pub event_id: StableEntityId,
    pub session_id: StableEntityId,
    pub event_sequence: u64,
    pub event_type: EventType,
    pub role: Option<EventRole>,
    pub occurred_at_unix_ms: Option<i64>,
    pub fidelity: Fidelity,
    /// A redacted, explicitly bounded excerpt. This is never body authority.
    pub bounded_preview: Option<String>,
    pub locator: SourceRecordLocator,
}

#[derive(Debug, Clone)]
pub struct RelationalFileTouchMetadata {
    pub file_touch_id: Uuid,
    pub event_id: Option<StableEntityId>,
    pub session_id: Option<StableEntityId>,
    pub path: String,
    pub old_path: Option<String>,
    pub change_kind: Option<FileChangeKind>,
    pub line_count_delta: Option<i64>,
    pub confidence: Confidence,
    pub created_at_unix_ms: Option<i64>,
    pub updated_at_unix_ms: Option<i64>,
}

/// A source-grouped, streaming relational input.
#[derive(Debug, Clone)]
pub enum RelationalProjectionRecord {
    BeginSource(RelationalSourceMetadata),
    Session(RelationalSessionMetadata),
    Event(RelationalEventMetadata),
    FileTouch(RelationalFileTouchMetadata),
    EndSource { source_id: Uuid },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationalProjectionStatus {
    Empty,
    Ready,
    Behind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationalProjectionMetadata {
    pub build_generation: u64,
    pub active_core_generation_id: Option<String>,
    pub target_core_generation_id: Option<String>,
    pub status: RelationalProjectionStatus,
    pub source_count: u64,
    pub session_count: u64,
    pub event_count: u64,
    pub file_touch_count: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationalProjectionReceipt {
    pub core_generation_id: String,
    pub build_generation: u64,
    pub source_count: u64,
    pub session_count: u64,
    pub event_count: u64,
    pub file_touch_count: u64,
}
