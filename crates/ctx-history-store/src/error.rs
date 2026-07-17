use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("record not found: {0}")]
    NotFound(Uuid),
    #[error("unsupported history store schema version: {0}")]
    UnsupportedSchemaVersion(i64),
    #[error("invalid import pending reason: {0}")]
    InvalidImportPendingReason(String),
    #[error("ctx import inventory schema is incompatible: {0}")]
    ImportInventorySchemaIncompatible(&'static str),
    #[error("unsafe or ambiguous history store identity")]
    UnsafeStoreIdentity,
    #[error("unsupported session history archive version: {0}")]
    UnsupportedArchiveVersion(u32),
    #[error(
        "ctx index is busy: WAL checkpoint could not complete ({log_frames} log frames, {checkpointed_frames} checkpointed)"
    )]
    WalCheckpointBusy {
        log_frames: i64,
        checkpointed_frames: i64,
    },
    #[error("ctx index is busy: another bulk search import is active")]
    BulkSearchImportBusy,
    #[error("bulk search guard belongs to a different ctx index")]
    InvalidBulkSearchGuard,
    #[error(
        "ctx search index rebuild is incomplete; run a writable command such as `ctx setup` or `ctx import` to resume it"
    )]
    SearchProjectionRebuildPending,
    #[error("invalid search projection rebuild phase: {0}")]
    InvalidSearchProjectionRebuildPhase(i64),
    #[error(
        "search projection rebuild unit is {bytes} bytes; bounded maximum is {max_bytes} bytes"
    )]
    SearchProjectionRebuildUnitTooLarge { bytes: usize, max_bytes: usize },
    #[error("ctx search projection rebuild made no progress within {timeout_ms}ms at its one-unit floor")]
    SearchProjectionRebuildTimedOut { timeout_ms: u64 },
    #[error("ctx search projection schema is incompatible: {0}")]
    SearchProjectionSchemaIncompatible(&'static str),
    #[error("ctx semantic document count is pending bounded maintenance")]
    SemanticSearchableItemCountPending,
    #[error("archive conflicts with existing {kind}: {id}")]
    ImportConflict { kind: &'static str, id: Uuid },
    #[error("archive artifact {id} content does not match its blob hash")]
    ArchiveArtifactHashMismatch { id: Uuid },
    #[error("unsafe blob path in local store: {0}")]
    UnsafeBlobPath(String),
    #[error("archive artifact {id} content byte size does not match archive metadata")]
    ArchiveArtifactSizeMismatch { id: Uuid },
    #[error("archive artifact {id} blob path is not canonical for its content hash")]
    ArchiveArtifactPathMismatch { id: Uuid },
    #[error("archive artifact {id} blob file is not a regular file: {path:?}")]
    ArchiveArtifactNonRegularFile { id: Uuid, path: PathBuf },
    #[error("archive artifact {id} is missing matching blob content")]
    ArchiveArtifactMissingContent { id: Uuid },
    #[error("provider event conflict for {provider}/{external_session_id} at index {provider_index}: existing hash {existing_hash}, new hash {new_hash}")]
    ProviderEventConflict {
        provider: String,
        external_session_id: String,
        provider_index: u64,
        existing_hash: String,
        new_hash: String,
    },
    #[error(
        "import inventory generation {expected_generation} for {provider} ({inventory_family}) was superseded"
    )]
    ImportInventorySuperseded {
        provider: String,
        inventory_family: &'static str,
        expected_generation: u64,
    },
    #[error("import observation changed for {provider} owner {owner_id}")]
    ProviderFileObservationChanged { provider: String, owner_id: String },
    #[error("invalid provider file checkpoint: {0}")]
    InvalidProviderFileCheckpoint(&'static str),
    #[error("provider file checkpoint for {provider} owner {owner_id} requires replacement")]
    ProviderFileCheckpointRequiresReplacement { provider: String, owner_id: String },
    #[error("provider file replacement has inconsistent seen {entity} ids")]
    ProviderFileReconciliationInconsistent { entity: &'static str },
    #[error("provider file replacement scope is invalid or no longer active")]
    InvalidProviderFilePublicationScope,
    #[error("provider file publication for {provider} owner {owner_id} is already active")]
    ProviderFileReplacementBusy { provider: String, owner_id: String },
    #[error(
        "provider file publication write does not belong to active owner {provider}/{owner_id}"
    )]
    ProviderFilePublicationOwnerMismatch { provider: String, owner_id: String },
    #[error("provider file replacement row limit {value} is outside 1..={max}")]
    ProviderFileReconciliationLimitOutOfRange { value: usize, max: usize },
    #[error("provider-session repair unit is {bytes} bytes; bounded maximum is {max_bytes} bytes")]
    ProviderSessionRepairUnitTooLarge { bytes: usize, max_bytes: usize },
    #[error("provider file replacement reconciliation is not complete")]
    ProviderFileReconciliationIncomplete,
    #[error("provider file publication staging is unavailable")]
    ProviderFileStaging,
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
    #[error("SQL result preview budget {estimated_bytes} bytes exceeds maximum {max_result_bytes}; lower max_rows, max_columns, or max_value_bytes")]
    RawSqlResultBudgetTooLarge {
        estimated_bytes: usize,
        max_result_bytes: usize,
    },
    #[error("SQL query timed out after {timeout_ms}ms")]
    RawSqlTimedOut { timeout_ms: u64 },
    #[error("bounded search lookup timed out after {timeout_ms}ms")]
    BoundedSearchTimedOut { timeout_ms: u64 },
}

impl StoreError {
    pub(crate) fn is_retryable_search_projection_recovery(&self) -> bool {
        match self {
            Self::WalCheckpointBusy { .. } | Self::BulkSearchImportBusy => true,
            Self::Sql(rusqlite::Error::SqliteFailure(error, _)) => matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ),
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;
