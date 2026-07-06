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
    #[error("unsupported session history archive version: {0}")]
    UnsupportedArchiveVersion(u32),
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
}

pub type Result<T> = std::result::Result<T, StoreError>;
