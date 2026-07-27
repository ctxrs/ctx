use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderJsonlInventoryLimit {
    Directories,
    Depth,
    EligiblePaths,
    MetadataEntries,
}

impl std::fmt::Display for ProviderJsonlInventoryLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Directories => "directories",
            Self::Depth => "depth",
            Self::EligiblePaths => "eligible_jsonl_paths",
            Self::MetadataEntries => "metadata_entries",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceFailureKind {
    NotFound,
    Permission,
    Locked,
    Corrupt,
    SchemaIncompatible,
    InvalidSource,
    SourceChanged,
    SourceDatabase,
    Io,
}

impl std::fmt::Display for ProviderSourceFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::NotFound => "not_found",
            Self::Permission => "permission",
            Self::Locked => "locked",
            Self::Corrupt => "corrupt",
            Self::SchemaIncompatible => "schema_incompatible",
            Self::InvalidSource => "invalid_source",
            Self::SourceChanged => "source_changed",
            Self::SourceDatabase => "source_database",
            Self::Io => "io",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Store(#[from] ctx_history_store::StoreError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("unsupported capture envelope schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("unsupported provider schema: {0}")]
    UnsupportedSchema(String),
    #[error("invalid capture payload: {0}")]
    InvalidPayload(String),
    #[error("invalid provider transcript path {path:?}: {reason}")]
    InvalidProviderTranscriptPath { path: PathBuf, reason: &'static str },
    #[error(
        "provider JSONL inventory exceeded {limit} limit: observed {observed}, maximum {maximum}"
    )]
    ProviderJsonlInventoryLimitExceeded {
        limit: ProviderJsonlInventoryLimit,
        maximum: usize,
        observed: usize,
    },
    #[error("{0} worker thread panicked")]
    WorkerPanicked(&'static str),
    #[error("system I/O error during {operation}: {source}")]
    SystemIo {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("system invariant failed: {0}")]
    SystemInvariant(&'static str),
    #[error("provider source changed during bounded capture")]
    SourceChangedDuringCapture,
    #[error("{provider} source {path:?} failed ({kind}): {detail}")]
    ProviderSource {
        provider: &'static str,
        path: PathBuf,
        kind: ProviderSourceFailureKind,
        detail: String,
    },
    #[error("provider cursor changed during bounded capture")]
    ProviderCursorConflict,
    #[error("line {line} in {path:?} is not a valid capture envelope: {source}")]
    InvalidJsonLine {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, CaptureError>;
