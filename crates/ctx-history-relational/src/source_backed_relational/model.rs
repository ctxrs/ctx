use std::collections::BTreeSet;

use ctx_history_core::{CoreRecord, SourceKey};
use thiserror::Error;
use uuid::Uuid;

pub const RELATIONAL_PROJECTION_SCHEMA_VERSION: u32 = 9;
pub const RELATIONAL_PROJECTION_CONTRACT_VERSION: u32 = 9;
pub const RELATIONAL_MATERIALIZER_REVISION: u32 = 5;

pub type Result<T> = std::result::Result<T, RelationalProjectionError>;

#[derive(Debug, Error)]
pub enum RelationalProjectionError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Core relational projection schema is missing")]
    MissingSchema,
    #[error(
        "unsupported Core relational schema {schema_version}, contract {contract_version}; rebuild the disposable relational projection"
    )]
    UnsupportedSchema {
        schema_version: i64,
        contract_version: i64,
    },
    #[error(
        "Core relational projection state is incompatible: {0}; rebuild the disposable relational projection"
    )]
    IncompatibleState(String),
    #[error("Core relational projection is missing stable view {0}")]
    MissingStableView(String),
    #[error(
        "Core relational WAL checkpoint remained busy ({busy} busy, {log_frames} log frames, {checkpointed_frames} checkpointed frames)"
    )]
    WalCheckpointBusy {
        busy: i64,
        log_frames: i64,
        checkpointed_frames: i64,
    },
    #[error("Core relational seal requested journal mode delete, SQLite selected {actual}")]
    UnexpectedJournalMode { actual: String },
    #[error(
        "Core SQL projection is missing at {projection_path} while a committed Core generation exists at {generation_path}; rebuild the relational projection from that generation"
    )]
    MissingSourceBackedSqlProjection {
        projection_path: std::path::PathBuf,
        generation_path: std::path::PathBuf,
    },
    #[error(
        "Core SQL projection is not ready for Core generation {expected_generation}; active generation is {active_generation:?} with status {status}; wait for daemon catch-up"
    )]
    SourceBackedSqlGenerationMismatch {
        expected_generation: String,
        active_generation: Option<String>,
        status: String,
    },
    #[error("invalid committed Core generation: {0}")]
    InvalidCoreGeneration(String),
    #[error("invalid Core relational record: {0}")]
    InvalidRecord(String),
    #[error("Core relational stream ordering violation: {0}")]
    InvalidStreamOrder(String),
    #[error("Core relational projection expected sources {expected:?}, received {received:?}")]
    SourceSetMismatch {
        expected: Vec<String>,
        received: Vec<String>,
    },
    #[error("Core relational source {source_id} expected {expected} events, received {received}")]
    SourceEventCountMismatch {
        source_id: String,
        expected: u64,
        received: u64,
    },
    #[error("Core relational generation expected {expected} events, projected {projected}")]
    GenerationEventCountMismatch { expected: u64, projected: u64 },
    #[error("Core relational count does not fit SQLite INTEGER: {0}")]
    CountOverflow(&'static str),
    #[error("SQL query is empty")]
    RawSqlEmpty,
    #[error("SQL query contains an interior NUL byte")]
    RawSqlInteriorNul,
    #[error("SQL query must be read-only")]
    RawSqlNotReadOnly,
    #[error("SQL query parameters are not supported")]
    RawSqlHasParameters,
    #[error("SQL query must return at least one column")]
    RawSqlNoColumns,
    #[error("SQL query returned {columns} columns; maximum is {max_columns}")]
    RawSqlTooManyColumns { columns: usize, max_columns: usize },
    #[error("{field} must be between {min} and {max}, got {value}")]
    RawSqlLimitOutOfRange {
        field: &'static str,
        value: usize,
        min: usize,
        max: usize,
    },
    #[error(
        "SQL result preview budget {estimated_bytes} bytes exceeds maximum {max_result_bytes}; lower max_rows, max_columns, or max_value_bytes"
    )]
    RawSqlResultBudgetTooLarge {
        estimated_bytes: usize,
        max_result_bytes: usize,
    },
    #[error("SQL query timed out after {timeout_ms}ms")]
    RawSqlTimedOut { timeout_ms: u64 },
}

/// One already-verified, immutable Core generation and its relational inputs.
///
/// This value contains no provider route, source locator, or body. Complete
/// records arrive separately from the pinned Core reader.
#[derive(Debug, Clone)]
pub struct CommittedCoreGeneration {
    pub generation_id: String,
    pub manifest_version: u32,
    pub core_record_version: u32,
    pub core_record_contract_fingerprint: String,
    pub lexical_schema_version: u32,
    pub policy_schema_hash: String,
    pub indexed_documents: u64,
    pub sources: Vec<RelationalSourceMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationalSourceHealth {
    Ready,
}

impl RelationalSourceHealth {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
        }
    }
}

/// Queryable source ownership metadata copied from the Core manifest.
#[derive(Debug, Clone)]
pub struct RelationalSourceMetadata {
    pub source: SourceKey,
    pub parser_revision: String,
    pub revision_digest: [u8; 32],
    pub indexed_event_count: u64,
    pub health: RelationalSourceHealth,
}

/// A source-grouped stream of complete generation-owned Core records.
#[derive(Debug, Clone)]
pub enum RelationalProjectionRecord {
    BeginSource(Box<RelationalSourceMetadata>),
    CoreRecord(Box<CoreRecord>),
    EndSource { source_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationalProjectionPlan {
    NoOp(RelationalProjectionReceipt),
    Rebuild,
    CatchUp { changed_source_ids: BTreeSet<Uuid> },
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
    pub active_manifest_version: Option<u32>,
    pub active_core_record_version: Option<u32>,
    pub active_core_record_contract_fingerprint: Option<String>,
    pub active_lexical_schema_version: Option<u32>,
    pub active_policy_schema_hash: Option<String>,
    pub active_materializer_revision: Option<u32>,
    pub target_core_generation_id: Option<String>,
    pub status: RelationalProjectionStatus,
    pub source_count: u64,
    pub session_count: u64,
    pub event_count: u64,
    pub repository_binding_count: u64,
    pub file_touch_count: u64,
    pub vcs_observation_count: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationalProjectionReceipt {
    pub core_generation_id: String,
    pub relational_schema_version: u32,
    pub materializer_revision: u32,
    pub build_generation: u64,
    pub source_count: u64,
    pub session_count: u64,
    pub event_count: u64,
    pub repository_binding_count: u64,
    pub file_touch_count: u64,
    pub vcs_observation_count: u64,
}

/// Generation frontiers for one internally consistent SQL read transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlSnapshot {
    pub relational_core_generation_id: Option<String>,
    pub relational_build_generation: u64,
    pub observed_core_generation_id: Option<String>,
    pub projection_status: RelationalProjectionStatus,
    pub stale: bool,
}
